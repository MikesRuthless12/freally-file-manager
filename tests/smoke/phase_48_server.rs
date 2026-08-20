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

    // ntfy does not go out as JSON at all — `WebhookSink::deliver`
    // posts a plain-text body to the configured topic URL (see
    // `webhook::send_ntfy`). This envelope is only the root-endpoint
    // form, and its `topic` is deliberately empty: the event carries no
    // topic, and filling it with `event.kind` meant publishing to a
    // topic named after the job type.
    let ntfy = format_webhook_payload(WebhookTarget::Ntfy, &ev);
    assert_eq!(ntfy.get("topic").and_then(|v| v.as_str()), Some(""));
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

    // PATCH was absent from the write-method deny-list, and dav-server
    // routes it to `handle_put` with `create = true` — so this created
    // and wrote a file on a "read-only" server, unauthenticated. The
    // gate is now an allow-list of the read verbs; every other verb,
    // including ones a future dav-server release adds, is refused.
    let patch = client
        .patch(format!("{base}/sneaky.bin"))
        .header("Content-Type", "application/x-sabredav-partialupdate")
        .header("X-Update-Range", "bytes=0-4")
        .body(b"PWNED".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(patch.status().as_u16(), 403, "read-only must reject PATCH");
    assert!(
        !dir.path().join("sneaky.bin").exists(),
        "PATCH must not create a file on a read-only server",
    );

    // Reads still work — the allow-list must not have over-rotated.
    let get = client.get(format!("{base}/")).send().await.unwrap();
    assert!(
        get.status().is_success() || get.status().as_u16() == 404,
        "read-only must still allow GET, got {}",
        get.status(),
    );

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
/// Multipart upload, end to end over the real HTTP surface.
///
/// The AWS SDKs and `aws-cli` switch to multipart above 8 MB without
/// being asked, so before this existed every large upload from a
/// default-configured client got a 405. Exercises the whole sequence:
/// create, two parts, complete — then checks the assembled object is
/// byte-exact and that the staging directory is gone.
#[tokio::test]
async fn s3_multipart_upload_roundtrip() {
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

    // Two distinguishable parts, so a wrong join order shows up as a
    // content mismatch rather than a length one.
    let part1: Vec<u8> = (0..40 * 1024usize).map(|i| (i % 251) as u8).collect();
    let part2: Vec<u8> = (0..24 * 1024usize).map(|i| (i % 97) as u8).collect();

    // 1. CreateMultipartUpload.
    let created = client
        .post(format!("{base}/bucket/big/obj.bin?uploads"))
        .send()
        .await
        .unwrap();
    assert!(
        created.status().is_success(),
        "create: {}",
        created.status()
    );
    let xml = created.text().await.unwrap();
    let upload_id =
        between(&xml, "<UploadId>", "</UploadId>").expect("the response must carry an UploadId");
    assert_eq!(upload_id.len(), 32, "upload id shape: {upload_id}");

    // 2. UploadPart, twice. Each answers with the ETag we must echo.
    let mut etags = Vec::new();
    for (n, body) in [(1usize, &part1), (2usize, &part2)] {
        let resp = client
            .put(format!(
                "{base}/bucket/big/obj.bin?partNumber={n}&uploadId={upload_id}"
            ))
            .body(body.clone())
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success(), "part {n}: {}", resp.status());
        let etag = resp
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .expect("a part must answer with an ETag")
            .to_string();
        etags.push(etag);
    }

    // The object must not exist yet — only completing publishes it.
    assert!(
        !dir.path().join("big").join("obj.bin").exists(),
        "the object must not appear until the upload is completed"
    );

    // 3. CompleteMultipartUpload.
    let complete_body = format!(
        "<CompleteMultipartUpload>\
         <Part><PartNumber>1</PartNumber><ETag>{}</ETag></Part>\
         <Part><PartNumber>2</PartNumber><ETag>{}</ETag></Part>\
         </CompleteMultipartUpload>",
        etags[0], etags[1],
    );
    let done = client
        .post(format!("{base}/bucket/big/obj.bin?uploadId={upload_id}"))
        .body(complete_body)
        .send()
        .await
        .unwrap();
    assert!(done.status().is_success(), "complete: {}", done.status());

    // The assembled object is the two parts, in order.
    let got = client
        .get(format!("{base}/bucket/big/obj.bin"))
        .send()
        .await
        .unwrap();
    assert!(got.status().is_success(), "GET: {}", got.status());
    let body = got.bytes().await.unwrap();
    let mut expected = part1.clone();
    expected.extend_from_slice(&part2);
    assert_eq!(
        body.as_ref(),
        expected.as_slice(),
        "the assembled object must be the parts joined in ascending order"
    );

    // Staging is cleaned up, and never appeared in a listing.
    assert!(
        !dir.path().join(".freally-uploads").exists(),
        "the staging directory must not outlive the upload"
    );
    let listing = client
        .get(format!("{base}/bucket"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(listing.contains("big/obj.bin"), "listing: {listing}");
    assert!(
        !listing.contains(".freally-uploads"),
        "in-flight upload staging must never be listed as an object: {listing}"
    );

    handle.shutdown().await;
}

/// A part the client never uploaded must not complete an object, and
/// an abort must take the staging with it.
#[tokio::test]
async fn s3_multipart_rejects_unknown_parts_and_aborts_cleanly() {
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

    let xml = client
        .post(format!("{base}/bucket/k.bin?uploads"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let upload_id = between(&xml, "<UploadId>", "</UploadId>").unwrap();

    // Completing with a part that was never sent must fail, not produce
    // a short object.
    let bad = client
        .post(format!("{base}/bucket/k.bin?uploadId={upload_id}"))
        .body(
            "<CompleteMultipartUpload><Part><PartNumber>7</PartNumber>\
             <ETag>\"deadbeef\"</ETag></Part></CompleteMultipartUpload>"
                .to_string(),
        )
        .send()
        .await
        .unwrap();
    assert!(bad.status().is_client_error(), "status {}", bad.status());
    assert!(!dir.path().join("k.bin").exists());

    // An unknown upload id is a 404, not a 500 or a traversal.
    let unknown = client
        .put(format!(
            "{base}/bucket/k.bin?partNumber=1&uploadId=../../etc"
        ))
        .body(vec![0u8; 4])
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status().as_u16(), 404, "a bad upload id must 404");

    // Abort removes the staging directory.
    let aborted = client
        .delete(format!("{base}/bucket/k.bin?uploadId={upload_id}"))
        .send()
        .await
        .unwrap();
    assert!(aborted.status().is_success(), "abort: {}", aborted.status());
    assert!(
        !dir.path()
            .join(".freally-uploads")
            .join(&upload_id)
            .exists(),
        "abort must remove the staging directory"
    );

    handle.shutdown().await;
}

/// Slice between two markers — the tests only need this much of an XML
/// reader.
fn between(haystack: &str, open: &str, close: &str) -> Option<String> {
    let start = haystack.find(open)? + open.len();
    let end = haystack[start..].find(close)? + start;
    Some(haystack[start..end].to_string())
}
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

const REGION: &str = "us-east-1";
const PAYLOAD_HASH: &str = "UNSIGNED-PAYLOAD";

/// `x-amz-date` for *now*, as `YYYYMMDDTHHMMSSZ`.
///
/// This used to be a hardcoded `20240101T000000Z`. The server folded
/// that value into the string-to-sign but never compared it to the
/// clock, so the test was replaying a years-old signature and passing —
/// encoding the very bug it should have caught. The server now enforces
/// AWS's ±15-minute window, so the test has to sign for the present.
fn amz_now() -> (String, String) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil_from_days (Howard Hinnant), the inverse of the server's parse.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let scope = format!("{y:04}{m:02}{d:02}");
    let stamp = format!(
        "{scope}T{:02}{:02}{:02}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    );
    (stamp, scope)
}

/// Compute an `Authorization: AWS4-HMAC-SHA256 …` header for a request,
/// using the same canonical-request → string-to-sign → signing-key → HMAC
/// chain the server verifies. Signs `host;x-amz-content-sha256;x-amz-date`.
fn sigv4_auth(
    akid: &str,
    secret: &str,
    host: &str,
    method: &str,
    path: &str,
    amz_date: &str,
    scope_date: &str,
) -> String {
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
        format!("host:{host}\nx-amz-content-sha256:{PAYLOAD_HASH}\nx-amz-date:{amz_date}\n");
    // Empty canonical query string for these requests.
    let canonical_request =
        format!("{method}\n{path}\n\n{canonical_headers}\n{signed_headers}\n{PAYLOAD_HASH}");
    let hashed_request = hex::encode(Sha256::digest(canonical_request.as_bytes()));
    let scope = format!("{scope_date}/{REGION}/s3/aws4_request");
    let string_to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{hashed_request}");

    let k_date = mac(format!("AWS4{secret}").as_bytes(), scope_date.as_bytes());
    let k_region = mac(&k_date, REGION.as_bytes());
    let k_service = mac(&k_region, b"s3");
    let k_signing = mac(&k_service, b"aws4_request");
    let signature = hex::encode(mac(&k_signing, string_to_sign.as_bytes()));

    format!(
        "AWS4-HMAC-SHA256 Credential={akid}/{scope_date}/{REGION}/s3/aws4_request, \
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
    let (amz_date, scope_date) = amz_now();

    // Correctly-signed PUT → accepted.
    let auth = sigv4_auth(
        "AKIDEXAMPLE",
        "secret",
        &host,
        "PUT",
        path,
        &amz_date,
        &scope_date,
    );
    let ok = client
        .put(format!("{base}{path}"))
        .header("host", &host)
        .header("x-amz-date", &amz_date)
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
    let bad = sigv4_auth(
        "AKIDEXAMPLE",
        "wrong-secret",
        &host,
        "PUT",
        path,
        &amz_date,
        &scope_date,
    );
    let wrong = client
        .put(format!("{base}{path}"))
        .header("host", &host)
        .header("x-amz-date", &amz_date)
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
        .header("x-amz-date", &amz_date)
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

/// A directory larger than one `readdir` batch must list completely.
///
/// `readdir` used to return the whole listing in a single
/// `SSH_FXP_NAME`. `russh_sftp` fills in a `longname` (an `ls -l` line)
/// beside each filename and attrs, so an entry costs ~110-160 bytes and
/// OpenSSH caps a message at `SFTP_MAX_MSG_LENGTH` (256 KiB) — `ls` on a
/// folder of more than roughly 2,000 files aborted the client with
/// "Received message too long". The server now emits bounded batches and
/// returns `Eof` only once the cursor is exhausted, which is the
/// protocol's intended shape.
///
/// Scope, stated honestly: 250 entries spans three 100-entry batches
/// but would NOT exceed OpenSSH's 256 KiB cap, so this does not
/// reproduce the original overflow — that needs >2,000 files and a real
/// OpenSSH client. What it does guard is the regression the batching
/// itself introduces: a cursor that fails to advance (infinite loop) or
/// advances wrongly (dropped or duplicated entries). That is the part
/// most likely to break, and it was previously untested because every
/// existing SFTP test lists a directory of one or two files.
#[tokio::test]
async fn sftp_readdir_pages_a_large_directory() {
    use std::sync::Arc;

    use russh_sftp::client::SftpSession;

    let dir = tempfile::tempdir().unwrap();
    const N: usize = 250;
    for i in 0..N {
        std::fs::write(dir.path().join(format!("f{i:04}.bin")), b"x").unwrap();
    }

    let cfg = ServerConfig {
        bind_addr: "127.0.0.1:0".into(),
        protocols: vec![Protocol::Sftp],
        auth: AuthMode::None,
        root: dir.path().to_path_buf(),
        readonly: false,
    };
    let handle = serve(cfg).await.expect("serve should bind SFTP");
    let addr = handle.local_addr();

    let mut ssh = russh::client::connect(
        Arc::new(russh::client::Config::default()),
        addr,
        AcceptAnyHostKey,
    )
    .await
    .expect("ssh connect");
    assert!(
        ssh.authenticate_password("anyuser", "")
            .await
            .expect("auth call")
            .success(),
        "AuthMode::None must accept",
    );

    let channel = ssh.channel_open_session().await.unwrap();
    channel.request_subsystem(true, "sftp").await.unwrap();
    let sftp = SftpSession::new(channel.into_stream())
        .await
        .expect("sftp handshake");

    // `read_dir` drives readdir until Eof, so it exercises the paging.
    let listed = sftp.read_dir(".").await.expect("read_dir");
    let names: Vec<String> = listed.map(|e| e.file_name()).collect();

    assert_eq!(
        names.len(),
        N,
        "every entry must be listed across batches, got {} of {N}",
        names.len(),
    );
    assert!(names.contains(&"f0000.bin".to_string()));
    assert!(
        names.contains(&"f0249.bin".to_string()),
        "the last entry must survive paging",
    );

    handle.shutdown().await;
}
