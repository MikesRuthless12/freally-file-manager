//! Phase 17d — live privilege-escalation spawn + handshake (caller side).
//!
//! When the unprivileged in-process `ElevatedRetry` attempt fails with
//! `err-permission-denied`, [`crate::commands::retry_elevated`] calls
//! [`elevated_retry`]: it creates a DACL-restricted named pipe (Windows)
//! / 0700 Unix socket, spawns `freally-helper` ELEVATED through the
//! OS-native consent flow (UAC `Start-Process -Verb RunAs` / `pkexec` /
//! `osascript … with administrator privileges`), and drives the JSON-RPC
//! handshake the helper binary speaks: `Hello` → `GrantCapabilities` →
//! `ElevatedRetry` → `Shutdown`.
//!
//! Security: the pipe/socket name is per-launch random (256 bits) and the
//! server is created BEFORE the spawn, so an attacker can't pre-connect;
//! on Windows the pipe DACL (see `freally_platform::secure_pipe`) admits
//! only the current user + Administrators.
//!
//! This crate (freally-ui) is the spawner's home: it is already async +
//! depends on tokio + freally-platform. The elevated `freally-helper`
//! binary stays `#![forbid(unsafe_code)]` and tokio-free; the unsafe DACL
//! FFI lives in `freally-platform`. The real consent dialog can't be
//! auto-tested — the wire path is covered non-elevated by
//! `tests/smoke/phase_17d_spawn.rs`; the elevated path is verified by
//! hand (see docs/ROADMAP.md Phase 17d).

use std::path::Path;
use std::time::Duration;

use freally_helper::capability::Capability;
use freally_helper::rpc::{PROTOCOL_VERSION, Request, Response, generate_pipe_name};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// How long to wait for the elevated helper to connect back after the
/// consent prompt fires. Covers a human clicking through UAC; a cancel
/// or timeout maps to `retry-elevated-unavailable` so the UI can't hang.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Failure modes of the elevated-retry spawn path. Both surface to the
/// frontend as the existing `retry-elevated-unavailable` key (the UI
/// shows one "couldn't elevate" affordance); the detail is for logs.
#[derive(Debug, thiserror::Error)]
pub enum ElevateError {
    /// Couldn't locate/spawn the helper, the user cancelled consent, or
    /// the connect timed out.
    #[error("elevation unavailable: {0}")]
    Unavailable(String),
    /// The pipe connected but the wire exchange failed (protocol
    /// mismatch, capability not granted, malformed/short line).
    #[error("handshake failed: {0}")]
    Handshake(String),
}

/// Spawn `freally-helper` elevated and run the `ElevatedRetry`
/// handshake for `src` → `dst`. Returns the helper's `Response`
/// (`ElevatedRetryOk` / `ElevatedRetryFailed` / `PathRejected` / …) so
/// the caller maps it exactly like the in-process path.
pub async fn elevated_retry(src: &Path, dst: &Path) -> Result<Response, ElevateError> {
    let pairs = [(src.to_path_buf(), dst.to_path_buf())];
    // One request in, one response out — `run_handshake` answers each
    // pair positionally, so this is *the* response, not merely the last.
    elevated_retry_batch(&pairs)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| ElevateError::Handshake("helper returned no response".into()))
}

/// FFM-M16 — run `ElevatedRetry` for **many** pairs across a single
/// elevated spawn, so the user answers **one** consent prompt for the
/// whole batch instead of one per file.
///
/// The helper's protocol is a request loop that runs until `Shutdown`
/// or EOF, so batching is a property of the driver, not a new wire
/// contract. Responses come back positionally: `responses[i]` answers
/// `pairs[i]`.
pub async fn elevated_retry_batch(
    pairs: &[(std::path::PathBuf, std::path::PathBuf)],
) -> Result<Vec<Response>, ElevateError> {
    if pairs.is_empty() {
        return Ok(Vec::new());
    }
    #[cfg(windows)]
    {
        elevated_retry_windows(pairs).await
    }
    #[cfg(all(unix, not(windows)))]
    {
        elevated_retry_unix(pairs).await
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = pairs;
        Err(ElevateError::Unavailable(
            "elevation unsupported on this platform".into(),
        ))
    }
}

#[cfg(windows)]
async fn elevated_retry_windows(
    pairs: &[(std::path::PathBuf, std::path::PathBuf)],
) -> Result<Vec<Response>, ElevateError> {
    use std::os::windows::process::CommandExt;

    // CREATE_NO_WINDOW: the relaunch shell shouldn't flash a console.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let pipe_name = generate_pipe_name(r"\\.\pipe\freally-helper-")
        .map_err(|e| ElevateError::Unavailable(format!("pipe name: {e}")))?;
    // Create the DACL-restricted server BEFORE spawning so the helper
    // can't connect before we listen and no other process can race in.
    let server = freally_platform::create_secure_named_pipe_server(&pipe_name)
        .map_err(|e| ElevateError::Unavailable(format!("pipe server: {e}")))?;

    let helper = sibling_helper()?;
    let (_program, args) =
        freally_helper::build_spawn_command(&helper, &pipe_name, &[Capability::ElevatedRetry]);

    // build_spawn_command yields ("powershell.exe", [...]); run it via an
    // ABSOLUTE powershell path to defend against PATH hijacking.
    let mut cmd = std::process::Command::new(absolute_powershell());
    cmd.args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    cmd.spawn()
        .map_err(|e| ElevateError::Unavailable(format!("spawn: {e}")))?;

    // Wait for the elevated child to connect (covers the UAC dialog). A
    // cancelled prompt never connects → the timeout fires.
    tokio::time::timeout(CONNECT_TIMEOUT, server.connect())
        .await
        .map_err(|_| ElevateError::Unavailable("connect timed out (consent cancelled?)".into()))?
        .map_err(|e| ElevateError::Unavailable(format!("connect: {e}")))?;

    run_handshake(server, pairs).await
}

#[cfg(all(unix, not(windows)))]
async fn elevated_retry_unix(
    pairs: &[(std::path::PathBuf, std::path::PathBuf)],
) -> Result<Vec<Response>, ElevateError> {
    use std::os::unix::fs::PermissionsExt;
    use tokio::net::UnixListener;

    // macOS `sun_path` is only ~104 bytes, so the socket path must stay
    // short. Bind in $XDG_RUNTIME_DIR when set (already a 0700 per-user
    // dir — /run/user/<uid> on Linux), else /tmp (short on both Linux and
    // macOS; NOT $TMPDIR, which is a long /var/folders path on macOS that
    // overflows SUN_LEN). No intermediate dir: the 64-hex random basename
    // is the unguessable moat (an attacker can't discover the path to race
    // our listener before the helper dials in, and even a winning race
    // yields no privilege — the racer can't perform the elevated copy), and
    // we tighten the socket node to 0600 once it exists.
    let sock_name = generate_pipe_name("freally-helper-")
        .map_err(|e| ElevateError::Unavailable(format!("socket name: {e}")))?;
    let base = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .filter(|d| !d.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    let sock_path = base.join(&sock_name);
    let listener = UnixListener::bind(&sock_path)
        .map_err(|e| ElevateError::Unavailable(format!("bind: {e}")))?;
    let _ = std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600));

    let helper = sibling_helper()?;
    let sock_str = sock_path.to_string_lossy().into_owned();
    let (program, args) =
        freally_helper::build_spawn_command(&helper, &sock_str, &[Capability::ElevatedRetry]);
    std::process::Command::new(&program)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| ElevateError::Unavailable(format!("spawn {program}: {e}")))?;

    let (stream, _addr) = tokio::time::timeout(CONNECT_TIMEOUT, listener.accept())
        .await
        .map_err(|_| ElevateError::Unavailable("accept timed out (consent cancelled?)".into()))?
        .map_err(|e| ElevateError::Unavailable(format!("accept: {e}")))?;
    // Best-effort: drop the socket node once the helper has connected.
    let _ = std::fs::remove_file(&sock_path);

    run_handshake(stream, pairs).await
}

/// Drive the handshake over a connected duplex stream and return one
/// `ElevatedRetry` response per requested pair, in order. Generic over
/// the stream type so the (Windows pipe) and (Unix socket) paths share
/// one driver.
///
/// `Hello` and `GrantCapabilities` happen once; the `ElevatedRetry`
/// step then repeats for every pair before the single `Shutdown`. That
/// is what makes FFM-M16's one-consent-per-batch possible — the OS
/// consent dialog belongs to the *spawn*, not to the request.
async fn run_handshake<S>(
    stream: S,
    pairs: &[(std::path::PathBuf, std::path::PathBuf)],
) -> Result<Vec<Response>, ElevateError>
where
    S: AsyncRead + AsyncWrite,
{
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);

    // 1. Hello → HelloOk (version must match).
    write_request(
        &mut write_half,
        &Request::Hello {
            version: PROTOCOL_VERSION,
        },
    )
    .await?;
    match read_response(&mut reader).await? {
        Response::HelloOk { version, .. } if version == PROTOCOL_VERSION => {}
        other => {
            return Err(ElevateError::Handshake(format!(
                "expected HelloOk v{PROTOCOL_VERSION}, got {other:?}"
            )));
        }
    }

    // 2. GrantCapabilities → CapabilitiesGranted (must include ElevatedRetry).
    write_request(
        &mut write_half,
        &Request::GrantCapabilities {
            capabilities: vec![Capability::ElevatedRetry],
        },
    )
    .await?;
    match read_response(&mut reader).await? {
        Response::CapabilitiesGranted { granted }
            if granted.contains(&Capability::ElevatedRetry) => {}
        other => {
            return Err(ElevateError::Handshake(format!(
                "ElevatedRetry capability not granted: {other:?}"
            )));
        }
    }

    // 3. One ElevatedRetry per pair, over the same connection.
    //
    // A failure partway through must NOT discard the answers already
    // collected: the helper has really copied those files, with
    // elevation, and throwing the results away would report them as
    // failures and tell the user to redo work that is already done.
    // The remainder is filled with an unavailable marker so the vector
    // stays 1:1 with `pairs` and the caller can attribute every row.
    let mut results = Vec::with_capacity(pairs.len());
    for (src, dst) in pairs {
        let sent = write_request(
            &mut write_half,
            &Request::ElevatedRetry {
                src: src.clone(),
                dst: dst.clone(),
            },
        )
        .await;
        let outcome = match sent {
            Ok(()) => read_response(&mut reader).await,
            Err(e) => Err(e),
        };
        match outcome {
            Ok(resp) => results.push(resp),
            Err(e) => {
                let reason = format!("elevated helper stopped responding: {e}");
                while results.len() < pairs.len() {
                    results.push(Response::CapabilityDenied {
                        reason: reason.clone(),
                    });
                }
                return Ok(results);
            }
        }
    }

    // 4. Shutdown — best-effort; the helper exits on the reply or EOF.
    let _ = write_request(&mut write_half, &Request::Shutdown).await;

    Ok(results)
}

/// Serialise a `Request` to one newline-terminated JSON line and write
/// it, matching `freally_helper::transport::write_line`.
async fn write_request<W>(w: &mut W, req: &Request) -> Result<(), ElevateError>
where
    W: AsyncWrite + Unpin,
{
    let mut buf =
        serde_json::to_vec(req).map_err(|e| ElevateError::Handshake(format!("serialize: {e}")))?;
    buf.push(b'\n');
    w.write_all(&buf)
        .await
        .map_err(|e| ElevateError::Handshake(format!("write: {e}")))?;
    w.flush()
        .await
        .map_err(|e| ElevateError::Handshake(format!("flush: {e}")))?;
    Ok(())
}

/// Read one newline-delimited JSON line and decode it as a `Response`,
/// matching `freally_helper::transport::read_line`.
async fn read_response<R>(r: &mut R) -> Result<Response, ElevateError>
where
    R: AsyncBufReadExt + Unpin,
{
    let mut line = String::new();
    let n = r
        .read_line(&mut line)
        .await
        .map_err(|e| ElevateError::Handshake(format!("read: {e}")))?;
    if n == 0 {
        return Err(ElevateError::Handshake("EOF before response".into()));
    }
    let trimmed = line.trim_end_matches(['\r', '\n']);
    serde_json::from_str(trimmed).map_err(|e| ElevateError::Handshake(format!("parse: {e}")))
}

/// Locate the `freally-helper` binary next to the running executable.
fn sibling_helper() -> Result<String, ElevateError> {
    let exe = std::env::current_exe()
        .map_err(|e| ElevateError::Unavailable(format!("current_exe: {e}")))?;
    let dir = exe
        .parent()
        .ok_or_else(|| ElevateError::Unavailable("executable has no parent dir".into()))?;
    #[cfg(windows)]
    let helper = dir.join("freally-helper.exe");
    #[cfg(not(windows))]
    let helper = dir.join("freally-helper");
    if !helper.exists() {
        return Err(ElevateError::Unavailable(format!(
            "helper binary not found at {}",
            helper.display()
        )));
    }
    Ok(helper.to_string_lossy().into_owned())
}

/// Resolve the absolute path to `powershell.exe` from `%SystemRoot%` so a
/// hijacked PATH can't substitute a malicious shell.
#[cfg(windows)]
fn absolute_powershell() -> String {
    match std::env::var("SystemRoot") {
        Ok(root) if !root.is_empty() => {
            format!(r"{root}\System32\WindowsPowerShell\v1.0\powershell.exe")
        }
        _ => "powershell.exe".to_string(),
    }
}
