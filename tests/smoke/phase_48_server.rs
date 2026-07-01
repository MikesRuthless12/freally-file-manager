//! Phase 48 smoke — server config/protocol serde, webhook payload shapes,
//! Prometheus exposition, and a real WebDAV PUT/GET round-trip that bumps
//! the `/metrics` counters.

use freally_server::{
    AuthMode, JobNotification, Metrics, OtelConfig, Protocol, ServerConfig, ServerError,
    WebhookTarget, format_webhook_payload, serve,
};

#[test]
fn config_and_protocol_round_trip() {
    let cfg = ServerConfig {
        bind_addr: "0.0.0.0:9000".into(),
        protocols: vec![Protocol::WebDav, Protocol::Http],
        auth: AuthMode::Bearer {
            token: "secret".into(),
        },
        root: "/srv/data".into(),
        readonly: true,
    };
    let json = serde_json::to_string(&cfg).unwrap();
    let back: ServerConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(cfg, back);
    // snake_case wire form for the protocol enum + tagged auth mode.
    assert!(
        json.contains("web_dav"),
        "expected snake_case protocol: {json}"
    );
    assert!(json.contains("\"mode\":\"bearer\""), "tagged auth: {json}");

    let otel: OtelConfig =
        serde_json::from_str(r#"{"endpoint":"http://localhost:4317","enabled":true}"#).unwrap();
    assert!(otel.enabled && otel.endpoint.contains("4317"));
}

#[test]
fn protocol_display_labels() {
    assert_eq!(Protocol::WebDav.to_string(), "WebDAV");
    assert_eq!(Protocol::Sftp.to_string(), "SFTP");
    assert_eq!(Protocol::S3.to_string(), "S3");
}

#[test]
fn webhook_payloads_carry_the_right_keys() {
    let ev = JobNotification {
        kind: "job_completed".into(),
        title: "Copy done".into(),
        body: "100 files".into(),
        ok: true,
    };
    let slack = format_webhook_payload(WebhookTarget::Slack, &ev);
    assert!(slack.get("text").and_then(|v| v.as_str()).is_some());

    let discord = format_webhook_payload(WebhookTarget::Discord, &ev);
    assert!(discord.get("content").is_some());

    let ntfy = format_webhook_payload(WebhookTarget::Ntfy, &ev);
    assert_eq!(
        ntfy.get("topic").and_then(|v| v.as_str()),
        Some("job_completed")
    );
    assert!(ntfy.get("message").is_some());

    let push = format_webhook_payload(WebhookTarget::Pushover, &ev);
    assert!(
        push.get("token").is_some() && push.get("user").is_some() && push.get("message").is_some()
    );
}

#[test]
fn prometheus_exposition_is_well_formed() {
    let m = Metrics {
        jobs_total: 3,
        files_copied_total: 100,
        bytes_copied_total: 4096,
        errors_total: 1,
        active_jobs: 2,
    };
    let s = m.render_prometheus();
    assert!(s.contains("# TYPE freally_jobs_total counter"));
    assert!(s.contains("freally_jobs_total 3"));
    assert!(s.contains("# TYPE freally_active_jobs gauge"));
    assert!(s.contains("freally_active_jobs 2"));
    for name in ["freally_jobs_total", "freally_active_jobs"] {
        let help = s.find(&format!("# HELP {name} ")).unwrap();
        let typ = s.find(&format!("# TYPE {name} ")).unwrap();
        let sample = s.find(&format!("\n{name} ")).unwrap();
        assert!(
            help < typ && typ < sample,
            "HELP/TYPE must precede the sample for {name}"
        );
    }
}

/// S3 is now served on its own axum router (see `s3_noauth_roundtrip`), but
/// it is a distinct transport: it can't share a bind with WebDAV/HTTP or
/// SFTP, and it has no bearer concept. Each such config is rejected up-front
/// with a `Bind` error rather than served in a surprising shape.
#[tokio::test]
async fn s3_mixed_or_bearer_configs_are_rejected() {
    // S3 + an HTTP-family protocol on one bind → Bind error.
    let mixed_http = ServerConfig {
        bind_addr: "127.0.0.1:0".into(),
        protocols: vec![Protocol::WebDav, Protocol::S3],
        ..Default::default()
    };
    assert!(
        matches!(serve(mixed_http).await, Err(ServerError::Bind { .. })),
        "S3 mixed with WebDAV/HTTP must be a Bind error"
    );

    // S3 + SFTP on one bind → Bind error.
    let mixed_sftp = ServerConfig {
        bind_addr: "127.0.0.1:0".into(),
        protocols: vec![Protocol::S3, Protocol::Sftp],
        ..Default::default()
    };
    assert!(
        matches!(serve(mixed_sftp).await, Err(ServerError::Bind { .. })),
        "S3 mixed with SFTP must be a Bind error"
    );

    // S3 + bearer auth → Bind error (S3 authenticates via SigV4, not bearer).
    let s3_bearer = ServerConfig {
        bind_addr: "127.0.0.1:0".into(),
        protocols: vec![Protocol::S3],
        auth: AuthMode::Bearer { token: "t".into() },
        ..Default::default()
    };
    assert!(
        matches!(serve(s3_bearer).await, Err(ServerError::Bind { .. })),
        "S3 with bearer auth must be a Bind error"
    );
}

/// The spec's acceptance test: PUT a 1 MiB file over WebDAV, GET it back
/// byte-equal, then confirm `/metrics` counted the write.
#[tokio::test]
async fn webdav_put_get_roundtrip_and_metrics() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        bind_addr: "127.0.0.1:0".into(),
        protocols: vec![Protocol::WebDav],
        auth: AuthMode::None,
        root: dir.path().to_path_buf(),
        readonly: false,
    };
    let handle = serve(cfg).await.expect("serve should bind");
    let base = format!("http://{}", handle.local_addr());
    let client = reqwest::Client::new();

    // Deterministic 1 MiB payload.
    let payload: Vec<u8> = (0..1024usize * 1024).map(|i| (i % 251) as u8).collect();

    let put = client
        .put(format!("{base}/file.bin"))
        .body(payload.clone())
        .send()
        .await
        .unwrap();
    assert!(put.status().is_success(), "PUT status {}", put.status());

    let got = client.get(format!("{base}/file.bin")).send().await.unwrap();
    assert!(got.status().is_success(), "GET status {}", got.status());
    let body = got.bytes().await.unwrap();
    assert_eq!(
        body.as_ref(),
        payload.as_slice(),
        "GET body must byte-match the PUT payload"
    );

    // The file really landed under the served root.
    assert!(dir.path().join("file.bin").is_file());

    // `/metrics` counted the write.
    let metrics = client
        .get(format!("{base}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(metrics.contains("# TYPE freally_jobs_total counter"));
    let jobs_line = metrics
        .lines()
        .find(|l| l.starts_with("freally_jobs_total "))
        .expect("jobs_total sample present");
    let n: u64 = jobs_line.rsplit(' ').next().unwrap().parse().unwrap();
    assert!(n >= 1, "expected >=1 job after PUT, got {n}");
    let bytes_line = metrics
        .lines()
        .find(|l| l.starts_with("freally_bytes_copied_total "))
        .unwrap();
    let bytes: u64 = bytes_line.rsplit(' ').next().unwrap().parse().unwrap();
    assert_eq!(bytes, payload.len() as u64, "bytes_copied_total");

    handle.shutdown().await;
}

/// A read-only server rejects writes with 403 before they touch disk.
#[tokio::test]
async fn readonly_rejects_writes() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        bind_addr: "127.0.0.1:0".into(),
        protocols: vec![Protocol::WebDav],
        readonly: true,
        root: dir.path().to_path_buf(),
        ..Default::default()
    };
    let handle = serve(cfg).await.expect("serve");
    let base = format!("http://{}", handle.local_addr());
    let client = reqwest::Client::new();

    let put = client
        .put(format!("{base}/nope.bin"))
        .body(vec![0u8; 16])
        .send()
        .await
        .unwrap();
    assert_eq!(put.status().as_u16(), 403, "read-only must reject PUT");
    assert!(!dir.path().join("nope.bin").exists());

    handle.shutdown().await;
}

/// Bearer auth: a missing/wrong token is 401, the right one passes.
#[tokio::test]
async fn bearer_auth_is_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        bind_addr: "127.0.0.1:0".into(),
        protocols: vec![Protocol::WebDav],
        auth: AuthMode::Bearer {
            token: "s3cr3t".into(),
        },
        root: dir.path().to_path_buf(),
        ..Default::default()
    };
    let handle = serve(cfg).await.expect("serve");
    let base = format!("http://{}", handle.local_addr());
    let client = reqwest::Client::new();

    // No credential → 401.
    let anon = client.get(format!("{base}/x")).send().await.unwrap();
    assert_eq!(anon.status().as_u16(), 401);

    // Correct bearer → reaches the filesystem (404 for a missing file is
    // a "passed auth" outcome).
    let authed = client
        .get(format!("{base}/x"))
        .bearer_auth("s3cr3t")
        .send()
        .await
        .unwrap();
    assert_ne!(authed.status().as_u16(), 401, "valid token must pass auth");

    // `/metrics` stays open for scrapers even with auth on.
    let metrics = client.get(format!("{base}/metrics")).send().await.unwrap();
    assert!(metrics.status().is_success(), "metrics open for scraping");

    handle.shutdown().await;
}

/// Minimal SFTP client SSH handler: the server's host key is ephemeral
/// (freshly generated each `serve`), so accept it unconditionally.
struct AcceptAnyHostKey;

impl russh::client::Handler for AcceptAnyHostKey {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// The SFTP acceptance test: stand up an SFTP-only server (Bearer auth),
/// connect a real russh + russh-sftp client, PUT a 64 KiB file, GET it back
/// byte-equal, and confirm the path jail rejects a `..` escape.
#[tokio::test]
async fn sftp_put_get_roundtrip() {
    use std::sync::Arc;

    use russh_sftp::client::SftpSession;
    use russh_sftp::protocol::OpenFlags;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let dir = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        bind_addr: "127.0.0.1:0".into(),
        protocols: vec![Protocol::Sftp],
        auth: AuthMode::Bearer {
            token: "secret".into(),
        },
        root: dir.path().to_path_buf(),
        readonly: false,
    };
    let handle = serve(cfg).await.expect("serve should bind SFTP");
    let addr = handle.local_addr();

    // Connect + authenticate. Bearer auth maps onto SSH password auth: any
    // username, password == token.
    let mut ssh = russh::client::connect(
        Arc::new(russh::client::Config::default()),
        addr,
        AcceptAnyHostKey,
    )
    .await
    .expect("ssh connect");
    let authed = ssh
        .authenticate_password("anyuser", "secret")
        .await
        .expect("auth call")
        .success();
    assert!(authed, "bearer token must authenticate over SFTP");

    // Open the SFTP subsystem.
    let channel = ssh.channel_open_session().await.unwrap();
    channel.request_subsystem(true, "sftp").await.unwrap();
    let sftp = SftpSession::new(channel.into_stream())
        .await
        .expect("sftp handshake");

    // Deterministic 64 KiB payload.
    let payload: Vec<u8> = (0..64 * 1024usize).map(|i| (i % 251) as u8).collect();

    // PUT.
    {
        let mut file = sftp
            .open_with_flags(
                "file.bin",
                OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE,
            )
            .await
            .expect("open for write");
        file.write_all(&payload).await.expect("write");
        file.shutdown().await.expect("close write handle");
    }
    // The file really landed under the served root.
    assert!(dir.path().join("file.bin").is_file());

    // GET it back, byte-for-byte.
    let got = {
        let mut file = sftp.open("file.bin").await.expect("open for read");
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).await.expect("read");
        buf
    };
    assert_eq!(
        got, payload,
        "SFTP GET body must byte-match the PUT payload"
    );

    // Path jail: a `..` escape is refused (and never written to disk).
    let escape = sftp
        .open_with_flags(
            "../escape.bin",
            OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE,
        )
        .await;
    assert!(
        escape.is_err(),
        "`..` traversal must be rejected by the jail"
    );
    assert!(
        !dir.path().join("..").join("escape.bin").exists(),
        "traversal target must not be created outside the root"
    );

    handle.shutdown().await;
}

// ---------------------------------------------------------------------------
// S3 surface
// ---------------------------------------------------------------------------

/// S3 acceptance test (open access): PUT a 64 KiB object, GET it back
/// byte-equal, confirm ListObjectsV2 XML carries the key, and prove the
/// path jail refuses an encoded `..` escape (4xx, nothing written).
#[tokio::test]
async fn s3_noauth_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        bind_addr: "127.0.0.1:0".into(),
        protocols: vec![Protocol::S3],
        auth: AuthMode::None,
        root: dir.path().to_path_buf(),
        readonly: false,
    };
    let handle = serve(cfg).await.expect("serve should bind S3");
    let base = format!("http://{}", handle.local_addr());
    let client = reqwest::Client::new();

    // Deterministic 64 KiB payload.
    let payload: Vec<u8> = (0..64 * 1024usize).map(|i| (i % 251) as u8).collect();

    // PutObject → 200, lands under the served root at the keyed path.
    let put = client
        .put(format!("{base}/bucket/dir/obj.bin"))
        .body(payload.clone())
        .send()
        .await
        .unwrap();
    assert!(put.status().is_success(), "PUT status {}", put.status());
    assert!(dir.path().join("dir").join("obj.bin").is_file());

    // GetObject → byte-equal body.
    let got = client
        .get(format!("{base}/bucket/dir/obj.bin"))
        .send()
        .await
        .unwrap();
    assert!(got.status().is_success(), "GET status {}", got.status());
    let body = got.bytes().await.unwrap();
    assert_eq!(
        body.as_ref(),
        payload.as_slice(),
        "GET body must byte-match the PUT payload"
    );

    // ListObjectsV2 → XML carries the key.
    let list = client
        .get(format!("{base}/bucket?list-type=2"))
        .send()
        .await
        .unwrap();
    assert!(list.status().is_success(), "LIST status {}", list.status());
    let xml = list.text().await.unwrap();
    assert!(
        xml.contains("<Key>dir/obj.bin</Key>"),
        "list XML must contain the key: {xml}"
    );
    assert!(
        xml.contains("<ListBucketResult"),
        "list must be a ListBucketResult: {xml}"
    );

    // Path jail: an encoded `..` escape is refused and never written outside
    // the served root.
    let escape = client
        .put(format!("{base}/bucket/..%2Fescape.bin"))
        .body(vec![0u8; 16])
        .send()
        .await
        .unwrap();
    assert!(
        escape.status().is_client_error(),
        "traversal PUT must be 4xx, got {}",
        escape.status()
    );
    assert!(
        !dir.path().parent().unwrap().join("escape.bin").exists(),
        "traversal target must not be created outside the root"
    );

    handle.shutdown().await;
}

/// Fixed SigV4 inputs the test signs with; the values themselves are
/// irrelevant to the maths as long as the client and server agree.
const AMZ_DATE: &str = "20240101T000000Z";
const SCOPE_DATE: &str = "20240101";
const REGION: &str = "us-east-1";
const PAYLOAD_HASH: &str = "UNSIGNED-PAYLOAD";

/// Compute an `Authorization: AWS4-HMAC-SHA256 …` header for a request,
/// using the same canonical-request → string-to-sign → signing-key → HMAC
/// chain the server verifies. Signs `host;x-amz-content-sha256;x-amz-date`.
fn sigv4_auth(akid: &str, secret: &str, host: &str, method: &str, path: &str) -> String {
    use hmac::{Hmac, Mac};
    use sha2::{Digest, Sha256};
    type HmacSha256 = Hmac<Sha256>;

    fn mac(key: &[u8], data: &[u8]) -> Vec<u8> {
        let mut m = HmacSha256::new_from_slice(key).unwrap();
        m.update(data);
        m.finalize().into_bytes().to_vec()
    }

    let signed_headers = "host;x-amz-content-sha256;x-amz-date";
    let canonical_headers =
        format!("host:{host}\nx-amz-content-sha256:{PAYLOAD_HASH}\nx-amz-date:{AMZ_DATE}\n");
    // Empty canonical query string for these requests.
    let canonical_request =
        format!("{method}\n{path}\n\n{canonical_headers}\n{signed_headers}\n{PAYLOAD_HASH}");
    let hashed_request = hex::encode(Sha256::digest(canonical_request.as_bytes()));
    let scope = format!("{SCOPE_DATE}/{REGION}/s3/aws4_request");
    let string_to_sign = format!("AWS4-HMAC-SHA256\n{AMZ_DATE}\n{scope}\n{hashed_request}");

    let k_date = mac(format!("AWS4{secret}").as_bytes(), SCOPE_DATE.as_bytes());
    let k_region = mac(&k_date, REGION.as_bytes());
    let k_service = mac(&k_region, b"s3");
    let k_signing = mac(&k_service, b"aws4_request");
    let signature = hex::encode(mac(&k_signing, string_to_sign.as_bytes()));

    format!(
        "AWS4-HMAC-SHA256 Credential={akid}/{SCOPE_DATE}/{REGION}/s3/aws4_request, \
         SignedHeaders={signed_headers}, Signature={signature}"
    )
}

/// S3 SigV4 auth: a correctly-signed PUT is accepted; a wrong signature and
/// a missing `Authorization` header are both 403.
#[tokio::test]
async fn s3_sigv4_auth() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        bind_addr: "127.0.0.1:0".into(),
        protocols: vec![Protocol::S3],
        auth: AuthMode::Basic {
            user: "AKIDEXAMPLE".into(),
            password: "secret".into(),
        },
        root: dir.path().to_path_buf(),
        readonly: false,
    };
    let handle = serve(cfg).await.expect("serve should bind S3");
    let host = handle.local_addr().to_string();
    let base = format!("http://{host}");
    let path = "/bucket/signed.bin";
    let client = reqwest::Client::new();

    // Correctly-signed PUT → accepted.
    let auth = sigv4_auth("AKIDEXAMPLE", "secret", &host, "PUT", path);
    let ok = client
        .put(format!("{base}{path}"))
        .header("host", &host)
        .header("x-amz-date", AMZ_DATE)
        .header("x-amz-content-sha256", PAYLOAD_HASH)
        .header("authorization", &auth)
        .body(b"hello sigv4".to_vec())
        .send()
        .await
        .unwrap();
    assert!(
        ok.status().is_success(),
        "valid SigV4 must be accepted, got {}",
        ok.status()
    );
    assert!(dir.path().join("signed.bin").is_file());

    // Wrong signature (signed with the wrong secret) → 403.
    let bad = sigv4_auth("AKIDEXAMPLE", "wrong-secret", &host, "PUT", path);
    let wrong = client
        .put(format!("{base}{path}"))
        .header("host", &host)
        .header("x-amz-date", AMZ_DATE)
        .header("x-amz-content-sha256", PAYLOAD_HASH)
        .header("authorization", &bad)
        .body(b"nope".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status().as_u16(), 403, "wrong signature must be 403");

    // No Authorization header → 403.
    let anon = client
        .put(format!("{base}{path}"))
        .header("host", &host)
        .header("x-amz-date", AMZ_DATE)
        .header("x-amz-content-sha256", PAYLOAD_HASH)
        .body(b"nope".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(
        anon.status().as_u16(),
        403,
        "missing Authorization must be 403"
    );

    handle.shutdown().await;
}
