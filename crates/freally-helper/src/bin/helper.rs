//! `freally-helper` binary entry point.
//!
//! Spawned by the main process via the OS-native elevation flow.
//! Speaks newline-delimited JSON-RPC over either (a) an endpoint the
//! caller created and still holds — `--pipe=` on Windows (a named
//! pipe with a restrictive DACL), `--port=` on Unix (a `127.0.0.1`
//! listener) — or (b) stdin/stdout when neither is given, which keeps
//! back-compat and the in-process tests working.
//!
//! The endpoint exists because elevation severs std-handle inheritance.
//! UAC's `Start-Process -Verb RunAs` on Windows, and `do shell script`
//! on macOS, both hand the child a fresh set, so it cannot be driven
//! over the stdio of the process that launched it.
//!
//! `--socket=<path>` is deliberately gone. A rendezvous by filesystem
//! path cannot be defended against a same-uid attacker: they unlink
//! the node and bind their own while the consent dialog is up, and
//! this process — running as root by then — connects to them and
//! does as it is told. `--port=` names a listener the parent already
//! holds, which nothing else can take over. See
//! `freally_helper::spawn` for the full reasoning.
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

    // Phase 17d — when the caller passes `--pipe=` (Windows named
    // pipe) or `--port=` (Unix loopback listener), dial that endpoint
    // instead of stdin/stdout. The elevated child cannot inherit the
    // parent's std handles, so it connects back to the rendezvous the
    // parent created and named on the argv.
    let endpoint = args.iter().find_map(|a| {
        a.strip_prefix("--pipe=")
            .map(|s| Endpoint::Pipe(s.to_string()))
            .or_else(|| {
                a.strip_prefix("--port=")
                    .map(|s| Endpoint::LoopbackPort(s.to_string()))
            })
    });

    let exit_code = match endpoint {
        Some(ref ep) => match run_over_endpoint(ep, &argv_requested) {
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

/// Where to dial back to. Shaped by the platform, because the two have
/// nothing in common beyond "the parent already owns it".
enum Endpoint {
    /// Windows named pipe path.
    Pipe(String),
    /// Decimal TCP port on `127.0.0.1`.
    LoopbackPort(String),
}

/// Defence-in-depth: the pipe's final path component must match the
/// `freally-helper-<64 hex>` shape `rpc::generate_pipe_name` produces,
/// so a tampered argv cannot point the helper at an arbitrary pipe
/// (e.g. a system one). The parent additionally restricts the pipe
/// DACL; this is the helper-side check.
fn pipe_name_ok(endpoint: &str) -> bool {
    let basename = endpoint.rsplit(['/', '\\']).next().unwrap_or(endpoint);
    parse_pipe_name("freally-helper-", basename).is_some()
}

/// The matching check for `--port=`: a real port, and loopback-only by
/// construction since the host is hard-coded rather than taken from
/// the argv. Port 0 is rejected — it means "assign me one", never a
/// destination.
fn parse_loopback_port(raw: &str) -> Option<u16> {
    match raw.parse::<u16>() {
        Ok(0) | Err(_) => None,
        Ok(p) => Some(p),
    }
}

/// Connect to the caller-created pipe / socket and drive the run-loop
/// over it; returns the process exit code. No unsafe, no tokio — the
/// client handle is a plain blocking `File` (Windows pipe) /
/// `UnixStream` (Unix), `try_clone`d to split read + write halves.
fn run_over_endpoint(endpoint: &Endpoint, argv_requested: &[Capability]) -> std::io::Result<i32> {
    match endpoint {
        Endpoint::Pipe(name) => {
            if !pipe_name_ok(name) {
                eprintln!("freally-helper: refusing pipe with unexpected name shape");
                return Ok(2);
            }
            let pipe = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(name)?;
            let mut reader = buf_reader(pipe.try_clone()?);
            let mut writer = pipe;
            finish(run_loop(&mut reader, &mut writer, argv_requested))
        }
        Endpoint::LoopbackPort(raw) => {
            let Some(port) = parse_loopback_port(raw) else {
                eprintln!("freally-helper: refusing endpoint that is not a port");
                return Ok(2);
            };
            // Hard-coded loopback: the argv carries a port and nothing
            // else, so a tampered argv cannot aim this process at a
            // host off the machine.
            let sock = std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))?;
            // Nagle would sit on our small newline-delimited frames
            // waiting for more to send; every message here is a
            // request or a reply someone is blocked on.
            let _ = sock.set_nodelay(true);
            let mut reader = buf_reader(sock.try_clone()?);
            let mut writer = sock;
            finish(run_loop(&mut reader, &mut writer, argv_requested))
        }
    }
}

/// Map a run-loop outcome onto the process exit code.
fn finish(outcome: Result<(), TransportError>) -> std::io::Result<i32> {
    match outcome {
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
        let retry = Request::ElevatedRetry { src, dst };
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

    /// Phase 17d — the pipe guard accepts only the
    /// `freally-helper-<64 hex>` basename shape and rejects arbitrary
    /// targets. Windows-only now: the Unix rendezvous is a port.
    #[test]
    fn pipe_name_ok_accepts_generated_shape_rejects_others() {
        let win = format!(r"\\.\pipe\freally-helper-{}", "a".repeat(64));
        assert!(pipe_name_ok(&win));
        assert!(pipe_name_ok(&format!("freally-helper-{}", "b".repeat(64))));
        // System pipe, arbitrary file, wrong-length suffix, non-hex:
        assert!(!pipe_name_ok(r"\\.\pipe\lsass"));
        assert!(!pipe_name_ok("/etc/passwd"));
        assert!(!pipe_name_ok("freally-helper-tooshort"));
        assert!(!pipe_name_ok(&format!("freally-helper-{}", "z".repeat(64))));
    }

    /// `--port=` is the whole of the Unix rendezvous, so what it
    /// accepts is a security boundary rather than a parsing detail.
    /// Port 0 means "assign me one" to `bind` and is never a
    /// destination; anything that is not a port must not reach
    /// `connect`; and the host is ours to choose, so an `addr:port`
    /// string must not be honoured either.
    #[test]
    fn loopback_port_accepts_only_a_real_port() {
        assert_eq!(parse_loopback_port("49871"), Some(49871));
        assert_eq!(parse_loopback_port("1"), Some(1));
        assert_eq!(parse_loopback_port("65535"), Some(65535));

        assert_eq!(
            parse_loopback_port("0"),
            None,
            "port 0 is not a destination"
        );
        assert_eq!(parse_loopback_port("65536"), None, "out of range");
        assert_eq!(parse_loopback_port("-1"), None);
        assert_eq!(parse_loopback_port(""), None);
        assert_eq!(parse_loopback_port("49871 "), None, "no stray whitespace");
        assert_eq!(parse_loopback_port("/tmp/freally-helper-abc"), None);
        assert_eq!(parse_loopback_port("127.0.0.1:49871"), None);
        assert_eq!(parse_loopback_port("10.0.0.5:49871"), None);
    }
}
