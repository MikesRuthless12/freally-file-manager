//! Phase 48 scaffold smoke — server config/protocol serde, webhook payload
//! shapes, Prometheus exposition, and the deferred `serve` contract.

use copythat_server::{
    JobNotification, Metrics, OtelConfig, Protocol, ServerConfig, ServerError, WebhookTarget,
    format_webhook_payload, serve,
};

#[test]
fn config_and_protocol_round_trip() {
    let cfg = ServerConfig {
        bind_addr: "0.0.0.0:9000".into(),
        protocols: vec![Protocol::WebDav, Protocol::Http],
        auth_token: Some("secret".into()),
    };
    let json = serde_json::to_string(&cfg).unwrap();
    let back: ServerConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(cfg, back);
    // snake_case wire form for the protocol enum.
    assert!(
        json.contains("web_dav"),
        "expected snake_case protocol: {json}"
    );

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
    assert!(s.contains("# TYPE copythat_jobs_total counter"));
    assert!(s.contains("copythat_jobs_total 3"));
    assert!(s.contains("# TYPE copythat_active_jobs gauge"));
    assert!(s.contains("copythat_active_jobs 2"));
    // HELP + TYPE must precede the sample line for each series.
    for name in ["copythat_jobs_total", "copythat_active_jobs"] {
        let help = s.find(&format!("# HELP {name} ")).unwrap();
        let typ = s.find(&format!("# TYPE {name} ")).unwrap();
        let sample = s.find(&format!("\n{name} ")).unwrap();
        assert!(
            help < typ && typ < sample,
            "HELP/TYPE must precede the sample for {name}"
        );
    }
}

#[test]
fn serve_is_deferred_for_each_protocol() {
    for p in [
        Protocol::WebDav,
        Protocol::Sftp,
        Protocol::Http,
        Protocol::S3,
    ] {
        let cfg = ServerConfig {
            protocols: vec![p],
            ..Default::default()
        };
        match serve(cfg) {
            Err(ServerError::NotImplemented { protocol }) => assert_eq!(protocol, p),
            other => panic!("expected NotImplemented for {p}, got {other:?}"),
        }
    }
}
