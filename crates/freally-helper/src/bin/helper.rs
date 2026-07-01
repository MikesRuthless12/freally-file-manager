//! `freally-helper` binary entry point.
//!
//! Spawned by the main process via the OS-native elevation flow.
//! Speaks newline-delimited JSON-RPC over either (a) a
//! `--pipe=`/`--socket=` endpoint the caller created with a
//! restrictive DACL / 0700 dir (Phase 17d — required when UAC's
//! `Start-Process -Verb RunAs` severs std-handle inheritance so the
//! elevated child can't be driven over stdio), or (b) stdin/stdout
//! when neither arg is given (back-compat + in-process tests).
//!
//! This binary is **never user-facing** — running it directly is
//! a no-op that reads from a tty and exits as soon as stdin
//! closes. The CLAUDE.md "executing actions with care" rule is
//! enforced by the capability allowlist + the Phase 17a path
//! safety bar; both run before any privileged action.

#![forbid(unsafe_code)]

use std::io::{BufWriter, stdin, stdout};

use freally_helper::capability::{Capability, parse_capability_list};
use freally_helper::handler::handle_request;
use freally_helper::rpc::{Request, Response, parse_pipe_name};
use freally_helper::transport::{TransportError, buf_reader, read_line, write_line};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let argv_requested = match resolve_capabilities(&args) {
        Ok(caps) => caps,
        Err(e) => {
            eprintln!("freally-helper: {e}");
            std::process::exit(2);
        }
    };

    // Phase 17d — when the caller passes `--pipe=` (Windows named pipe)
    // or `--socket=` (Unix socket), dial that endpoint instead of
    // stdin/stdout. The elevated child can't inherit the parent's std
    // handles through UAC, so it connects back over the per-launch
    // random pipe/socket the parent created + named on the argv.
    let endpoint = args.iter().find_map(|a| {
        a.strip_prefix("--pipe=")
            .or_else(|| a.strip_prefix("--socket="))
            .map(|s| s.to_string())
    });

    let exit_code = match endpoint {
        Some(ep) => match run_over_endpoint(&ep, &argv_requested) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("freally-helper: endpoint error: {e}");
                4
            }
        },
        None => {
            let mut reader = buf_reader(stdin().lock());
            let mut writer = BufWriter::new(stdout().lock());
            match run_loop(&mut reader, &mut writer, &argv_requested) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("freally-helper: transport error: {e}");
                    3
                }
            }
        }
    };
    std::process::exit(exit_code);
}

/// Defence-in-depth: the endpoint's final path component must match the
/// `freally-helper-<64 hex>` shape `rpc::generate_pipe_name` produces,
/// so a tampered argv can't point the helper at an arbitrary pipe /
/// socket (e.g. a system pipe). The parent additionally restricts the
/// pipe DACL / socket-dir perms; this is the helper-side check.
fn endpoint_name_ok(endpoint: &str) -> bool {
    let basename = endpoint.rsplit(['/', '\\']).next().unwrap_or(endpoint);
    parse_pipe_name("freally-helper-", basename).is_some()
}

/// Connect to the caller-created pipe / socket and drive the run-loop
/// over it; returns the process exit code. No unsafe, no tokio — the
/// client handle is a plain blocking `File` (Windows pipe) /
/// `UnixStream` (Unix), `try_clone`d to split read + write halves.
fn run_over_endpoint(endpoint: &str, argv_requested: &[Capability]) -> std::io::Result<i32> {
    if !endpoint_name_ok(endpoint) {
        eprintln!("freally-helper: refusing endpoint with unexpected name shape");
        return Ok(2);
    }

    #[cfg(windows)]
    let (mut reader, mut writer) = {
        let pipe = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(endpoint)?;
        (buf_reader(pipe.try_clone()?), pipe)
    };
    #[cfg(unix)]
    let (mut reader, mut writer) = {
        let sock = std::os::unix::net::UnixStream::connect(endpoint)?;
        (buf_reader(sock.try_clone()?), sock)
    };

    match run_loop(&mut reader, &mut writer, argv_requested) {
        Ok(()) => Ok(0),
        Err(e) => {
            eprintln!("freally-helper: transport error: {e}");
            Ok(3)
        }
    }
}

fn resolve_capabilities(args: &[String]) -> Result<Vec<Capability>, String> {
    let raw = args
        .iter()
        .find_map(|a| a.strip_prefix("--capabilities=").map(|s| s.to_string()));
    match raw {
        Some(list) => parse_capability_list(&list),
        // Default-empty grants only Hello + Shutdown (lifecycle).
        // The caller MUST explicitly opt in to elevated paths.
        None => Ok(Vec::new()),
    }
}

/// Intersect `argv_requested` (the upper bound) with
/// `pipe_granted` (the lower bound) so a `GrantCapabilities`
/// request can never widen beyond what the spawn argv asked for.
fn effective_capabilities(
    argv_requested: &[Capability],
    pipe_granted: &[Capability],
) -> Vec<Capability> {
    let mut out: Vec<Capability> = Vec::with_capacity(pipe_granted.len());
    for cap in pipe_granted {
        if argv_requested.contains(cap) {
            out.push(*cap);
        }
    }
    out
}

fn run_loop<R: std::io::BufRead, W: std::io::Write>(
    reader: &mut R,
    writer: &mut W,
    argv_requested: &[Capability],
) -> Result<(), TransportError> {
    // Phase 17j — runtime-granted starts empty. The caller must
    // send `Request::GrantCapabilities` over the (DACL-restricted)
    // pipe before any capability-bearing request is accepted.
    // argv `--capabilities=` is the upper bound; this is the
    // lower bound. Effective = argv ∩ pipe.
    let mut pipe_granted: Vec<Capability> = Vec::new();
    loop {
        let request: Request = match read_line(reader) {
            Ok(r) => r,
            Err(TransportError::Eof) => {
                // Caller closed the pipe — exit cleanly.
                return Ok(());
            }
            Err(TransportError::Serde(e)) => {
                // Malformed JSON. Surface a typed Failed response so
                // the caller knows the helper saw the line; do NOT
                // propagate the parse error on its own — that would
                // tear down the connection on the first hiccup.
                let resp = Response::Failed {
                    localized_key: "err-helper-invalid-json".into(),
                    message: e.to_string(),
                };
                write_line(writer, &resp)?;
                continue;
            }
            Err(other) => return Err(other),
        };

        let is_shutdown = matches!(request, Request::Shutdown);
        // GrantCapabilities is special-cased here in the binary
        // because it mutates the per-session granted state; every
        // other request gates through the stateless
        // `handle_request`.
        let response = match &request {
            Request::GrantCapabilities { capabilities } => {
                pipe_granted = capabilities.clone();
                let granted = effective_capabilities(argv_requested, &pipe_granted);
                Response::CapabilitiesGranted { granted }
            }
            _ => {
                let effective = effective_capabilities(argv_requested, &pipe_granted);
                handle_request(&request, &effective)
            }
        };
        write_line(writer, &response)?;
        if is_shutdown {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    // Integration-style test that pumps a synthetic request stream
    // through the run-loop without spawning the binary. Exercises
    // the malformed-line recovery path.
    use super::*;
    use std::io::{BufReader, Cursor};

    #[test]
    fn run_loop_handles_malformed_then_valid_line() {
        let request_line = serde_json::to_string(&Request::Shutdown).unwrap();
        let stream = format!("not json\n{request_line}\n");
        let mut reader = BufReader::new(Cursor::new(stream.into_bytes()));
        let mut wire: Vec<u8> = Vec::new();
        run_loop(&mut reader, &mut wire, &[]).unwrap();
        // Response stream should carry Failed (for the bad line) +
        // ShuttingDown (for the valid Shutdown).
        let body = String::from_utf8(wire).unwrap();
        let mut lines = body.lines();
        let r1: Response = serde_json::from_str(lines.next().unwrap()).unwrap();
        let r2: Response = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert!(matches!(r1, Response::Failed { .. }));
        assert!(matches!(r2, Response::ShuttingDown));
    }

    /// Phase 17j — capability-bearing requests sent before
    /// `GrantCapabilities` must surface as `CapabilityDenied`,
    /// regardless of what `--capabilities=` argv said. The argv
    /// is the upper bound; the lower bound starts at zero.
    #[test]
    fn run_loop_denies_capability_request_before_grant() {
        let req = Request::ElevatedRetry {
            src: std::path::PathBuf::from("/tmp/src"),
            dst: std::path::PathBuf::from("/tmp/dst"),
        };
        let req_line = serde_json::to_string(&req).unwrap();
        let shut_line = serde_json::to_string(&Request::Shutdown).unwrap();
        let stream = format!("{req_line}\n{shut_line}\n");
        let mut reader = BufReader::new(Cursor::new(stream.into_bytes()));
        let mut wire: Vec<u8> = Vec::new();
        // argv-requested = ElevatedRetry, but no GrantCapabilities
        // landed yet, so effective = ∅.
        run_loop(&mut reader, &mut wire, &[Capability::ElevatedRetry]).unwrap();
        let body = String::from_utf8(wire).unwrap();
        let mut lines = body.lines();
        let r1: Response = serde_json::from_str(lines.next().unwrap()).unwrap();
        let r2: Response = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert!(
            matches!(r1, Response::CapabilityDenied { .. }),
            "expected CapabilityDenied before GrantCapabilities, got {r1:?}"
        );
        assert!(matches!(r2, Response::ShuttingDown));
    }

    /// Phase 17j — `GrantCapabilities` populates the per-session
    /// state; subsequent requests use the intersection of argv-
    /// requested and pipe-granted.
    #[test]
    fn run_loop_grants_then_serves_capability() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.bin");
        let dst = dir.path().join("dst.bin");
        std::fs::write(&src, b"payload").unwrap();
        let grant = Request::GrantCapabilities {
            capabilities: vec![Capability::ElevatedRetry],
        };
        let retry = Request::ElevatedRetry {
            src: src.clone(),
            dst: dst.clone(),
        };
        let stream = format!(
            "{}\n{}\n{}\n",
            serde_json::to_string(&grant).unwrap(),
            serde_json::to_string(&retry).unwrap(),
            serde_json::to_string(&Request::Shutdown).unwrap(),
        );
        let mut reader = BufReader::new(Cursor::new(stream.into_bytes()));
        let mut wire: Vec<u8> = Vec::new();
        run_loop(&mut reader, &mut wire, &[Capability::ElevatedRetry]).unwrap();
        let body = String::from_utf8(wire).unwrap();
        let mut lines = body.lines();
        let r1: Response = serde_json::from_str(lines.next().unwrap()).unwrap();
        let r2: Response = serde_json::from_str(lines.next().unwrap()).unwrap();
        let r3: Response = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert!(matches!(r1, Response::CapabilitiesGranted { .. }));
        assert!(
            matches!(r2, Response::ElevatedRetryOk { bytes: 7 }),
            "expected ElevatedRetryOk after grant, got {r2:?}"
        );
        assert!(matches!(r3, Response::ShuttingDown));
    }

    /// Phase 17j — pipe-granted set wider than argv-requested is
    /// silently clamped to argv (you can't widen the grant beyond
    /// what the spawn argv asked for). A request for the
    /// non-argv-requested capability after such a clamp is denied.
    #[test]
    fn run_loop_clamps_grant_to_argv_upper_bound() {
        let grant = Request::GrantCapabilities {
            capabilities: vec![Capability::ElevatedRetry, Capability::HardwareErase],
        };
        let erase = Request::HardwareErase {
            device: std::path::PathBuf::from("/dev/nvme0n1"),
        };
        let stream = format!(
            "{}\n{}\n{}\n",
            serde_json::to_string(&grant).unwrap(),
            serde_json::to_string(&erase).unwrap(),
            serde_json::to_string(&Request::Shutdown).unwrap(),
        );
        let mut reader = BufReader::new(Cursor::new(stream.into_bytes()));
        let mut wire: Vec<u8> = Vec::new();
        // argv-requested = ElevatedRetry only. The grant asked for
        // HardwareErase too but the intersection drops it.
        run_loop(&mut reader, &mut wire, &[Capability::ElevatedRetry]).unwrap();
        let body = String::from_utf8(wire).unwrap();
        let mut lines = body.lines();
        let r1: Response = serde_json::from_str(lines.next().unwrap()).unwrap();
        let r2: Response = serde_json::from_str(lines.next().unwrap()).unwrap();
        let _r3: Response = serde_json::from_str(lines.next().unwrap()).unwrap();
        match r1 {
            Response::CapabilitiesGranted { granted } => {
                assert_eq!(granted, vec![Capability::ElevatedRetry]);
            }
            other => panic!("expected CapabilitiesGranted, got {other:?}"),
        }
        assert!(
            matches!(r2, Response::CapabilityDenied { .. }),
            "HardwareErase must be denied even after over-broad grant: {r2:?}"
        );
    }

    /// Helper unit-test: `effective_capabilities` is intersection.
    #[test]
    fn effective_capabilities_is_intersection() {
        let argv = [Capability::ElevatedRetry, Capability::ShellExtension];
        let pipe = vec![Capability::ElevatedRetry, Capability::HardwareErase];
        let eff = effective_capabilities(&argv, &pipe);
        assert_eq!(eff, vec![Capability::ElevatedRetry]);
    }

    /// Phase 17d — the endpoint guard accepts only the
    /// `freally-helper-<64 hex>` basename shape, on either a Windows
    /// pipe path or a Unix socket path, and rejects arbitrary targets.
    #[test]
    fn endpoint_name_ok_accepts_generated_shape_rejects_others() {
        let win = format!(r"\\.\pipe\freally-helper-{}", "a".repeat(64));
        let unix = format!("/run/user/1000/freally-helper-{}", "b".repeat(64));
        assert!(endpoint_name_ok(&win));
        assert!(endpoint_name_ok(&unix));
        // System pipe, arbitrary file, wrong-length suffix:
        assert!(!endpoint_name_ok(r"\\.\pipe\lsass"));
        assert!(!endpoint_name_ok("/etc/passwd"));
        assert!(!endpoint_name_ok("freally-helper-tooshort"));
        assert!(!endpoint_name_ok(&format!(
            "freally-helper-{}",
            "z".repeat(64)
        )));
    }
}
