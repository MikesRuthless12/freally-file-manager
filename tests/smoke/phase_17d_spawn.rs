//! Phase 17d — non-elevated handshake smoke for the elevated-retry path.
//!
//! Spawns the REAL `freally-helper` binary NON-elevated with
//! `--pipe=` / `--socket=` + `--capabilities=elevated_retry`, creates the
//! matching server end (Windows: the DACL-restricted pipe from
//! `freally-platform`; Unix: a tokio `UnixListener` in a 0700 tempdir),
//! and drives the exact `Hello` → `GrantCapabilities` → `ElevatedRetry` →
//! `Shutdown` handshake the production caller (`freally-ui::elevate`)
//! uses. This validates the binary's `--pipe`/`--socket` mode + run-loop +
//! wire protocol end-to-end WITHOUT the UAC/polkit/osascript consent
//! (which can't be automated — that path is verified by hand).
//!
//! Gated behind the `spawn-tests` feature (binary-spawning + named-pipe
//! I/O): `cargo test -p freally-helper --features spawn-tests`.
#![cfg(feature = "spawn-tests")]

use std::process::Command;
use std::time::Duration;

use freally_helper::capability::Capability;
#[cfg(windows)]
use freally_helper::rpc::generate_pipe_name;
use freally_helper::rpc::{PROTOCOL_VERSION, Request, Response};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

const HELPER: &str = env!("CARGO_BIN_EXE_freally-helper");
const TIMEOUT: Duration = Duration::from_secs(10);

/// Write one newline-terminated JSON request (matches the helper's
/// `transport::write_line`).
async fn send<W: AsyncWrite + Unpin>(w: &mut W, req: &Request) {
    let mut buf = serde_json::to_vec(req).unwrap();
    buf.push(b'\n');
    w.write_all(&buf).await.unwrap();
    w.flush().await.unwrap();
}

/// Read one newline-delimited JSON `Response`.
async fn recv<R: AsyncBufReadExt + Unpin>(r: &mut R) -> Response {
    let mut line = String::new();
    let n = r.read_line(&mut line).await.unwrap();
    assert!(n > 0, "EOF before response");
    serde_json::from_str(line.trim_end_matches(['\r', '\n'])).unwrap()
}

/// Create the server end, spawn the (non-elevated) helper wired to it,
/// and return the connected duplex stream + the child handle.
#[cfg(windows)]
async fn connect_helper(caps: &str) -> (impl AsyncRead + AsyncWrite, std::process::Child) {
    let pipe = generate_pipe_name(r"\\.\pipe\freally-helper-").unwrap();
    let server = freally_platform::create_secure_named_pipe_server(&pipe).unwrap();
    let child = Command::new(HELPER)
        .arg(format!("--pipe={pipe}"))
        .arg(format!("--capabilities={caps}"))
        .spawn()
        .expect("spawn helper");
    tokio::time::timeout(TIMEOUT, server.connect())
        .await
        .expect("helper did not connect in time")
        .expect("pipe connect failed");
    (server, child)
}

#[cfg(all(unix, not(windows)))]
async fn connect_helper(caps: &str) -> (impl AsyncRead + AsyncWrite, std::process::Child) {
    // Loopback, not a socket file. The helper no longer accepts
    // `--socket=<path>`: a filesystem rendezvous can be unlinked and
    // re-bound by any process running as the same uid while the consent
    // dialog is up, which handed the root helper to the attacker. This
    // mirrors what `elevate.rs` now does — bind first, hold the
    // listener, pass only the port.
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind loopback");
    let port = listener.local_addr().expect("local_addr").port();
    let child = Command::new(HELPER)
        .arg(format!("--port={port}"))
        .arg(format!("--capabilities={caps}"))
        .spawn()
        .expect("spawn helper");
    let (stream, _addr) = tokio::time::timeout(TIMEOUT, listener.accept())
        .await
        .expect("helper did not connect in time")
        .expect("accept failed");
    (stream, child)
}

#[tokio::test]
async fn elevated_retry_handshake_copies_file_non_elevated() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    let payload = b"phase-17d-spawn-payload";
    std::fs::write(&src, payload).unwrap();

    let (stream, mut child) = connect_helper("elevated_retry").await;
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);

    send(
        &mut write_half,
        &Request::Hello {
            version: PROTOCOL_VERSION,
        },
    )
    .await;
    assert!(matches!(recv(&mut reader).await, Response::HelloOk { .. }));

    send(
        &mut write_half,
        &Request::GrantCapabilities {
            capabilities: vec![Capability::ElevatedRetry],
        },
    )
    .await;
    match recv(&mut reader).await {
        Response::CapabilitiesGranted { granted } => {
            assert!(granted.contains(&Capability::ElevatedRetry));
        }
        other => panic!("expected CapabilitiesGranted, got {other:?}"),
    }

    send(
        &mut write_half,
        &Request::ElevatedRetry {
            src: src.clone(),
            dst: dst.clone(),
        },
    )
    .await;
    match recv(&mut reader).await {
        Response::ElevatedRetryOk { bytes } => assert_eq!(bytes, payload.len() as u64),
        other => panic!("expected ElevatedRetryOk, got {other:?}"),
    }

    send(&mut write_half, &Request::Shutdown).await;
    assert!(matches!(recv(&mut reader).await, Response::ShuttingDown));

    let _ = child.wait();
    assert_eq!(std::fs::read(&dst).unwrap(), payload, "dst must match src");
}

#[tokio::test]
async fn elevated_retry_denied_before_grant() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    std::fs::write(&src, b"x").unwrap();

    let (stream, mut child) = connect_helper("elevated_retry").await;
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);

    send(
        &mut write_half,
        &Request::Hello {
            version: PROTOCOL_VERSION,
        },
    )
    .await;
    assert!(matches!(recv(&mut reader).await, Response::HelloOk { .. }));

    // Skip GrantCapabilities — a capability-bearing request before the
    // grant must be refused regardless of the spawn argv (Phase 17j).
    send(
        &mut write_half,
        &Request::ElevatedRetry {
            src,
            dst: dst.clone(),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut reader).await,
        Response::CapabilityDenied { .. }
    ));

    send(&mut write_half, &Request::Shutdown).await;
    assert!(matches!(recv(&mut reader).await, Response::ShuttingDown));

    let _ = child.wait();
    assert!(!dst.exists(), "a denied retry must not copy anything");
}
