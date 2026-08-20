//! Phase 48 — webhook delivery (Slack / Discord / ntfy.sh / Pushover).
//!
//! [`crate::format_webhook_payload`] builds each service's JSON body; this
//! module POSTs it. A [`WebhookSink`] is one configured destination, and
//! [`WebhookSink::deliver`] formats + sends a [`JobNotification`] to it.

use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::{JobNotification, ServerError, WebhookTarget, format_webhook_payload};

/// Pushover requires an application token + user key alongside the message
/// (the other services authenticate via the secret in the webhook URL).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushoverCreds {
    pub token: String,
    pub user: String,
}

/// One configured webhook destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookSink {
    /// Which service's payload shape to use.
    pub target: WebhookTarget,
    /// Full webhook URL (Slack / Discord incoming-webhook URL, an ntfy
    /// topic URL, or the Pushover messages endpoint).
    pub url: String,
    /// Pushover app token + user key; ignored for the other targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pushover: Option<PushoverCreds>,
}

impl WebhookSink {
    /// Format `event` for this sink's target and POST it. Returns
    /// [`ServerError::Webhook`] on a transport error or a non-2xx response.
    pub async fn deliver(
        &self,
        client: &Client,
        event: &JobNotification,
    ) -> Result<(), ServerError> {
        // ntfy takes a plain-text body at a topic URL, not JSON.
        if self.target == WebhookTarget::Ntfy {
            return send_ntfy(client, &self.url, event).await;
        }
        let mut payload = format_webhook_payload(self.target, event);
        // Pushover's token/user come from config, not the event body.
        if self.target == WebhookTarget::Pushover {
            if let (Some(creds), Some(obj)) = (&self.pushover, payload.as_object_mut()) {
                obj.insert("token".into(), creds.token.clone().into());
                obj.insert("user".into(), creds.user.clone().into());
            }
        }
        send_webhook(client, &self.url, &payload).await
    }
}

/// POST a notification to an ntfy *topic* URL.
///
/// ntfy is not a JSON-webhook service. It accepts a JSON envelope only
/// at the server **root** (`https://ntfy.sh/`, with the topic as a
/// field); POSTed to a topic URL — which is what [`WebhookSink::url`]
/// is documented to hold — the JSON body is taken verbatim as the
/// message text, so the device displayed a raw `{"topic":...}` object
/// instead of the notification. The plain-text body plus `Title` /
/// `Priority` / `Tags` headers is the form that matches a topic URL,
/// and is what this repo's own `notify-ntfy` plugin already sends.
pub async fn send_ntfy(
    client: &Client,
    url: &str,
    event: &JobNotification,
) -> Result<(), ServerError> {
    let req = client
        .post(url)
        .header("Title", header_safe(&event.title))
        .header("Priority", if event.ok { "default" } else { "high" })
        .header(
            "Tags",
            if event.ok {
                "white_check_mark,freally"
            } else {
                "rotating_light,freally"
            },
        )
        .body(event.body.clone());
    send_request(url, req).await
}

/// ntfy carries the title in an HTTP header, which cannot hold control
/// characters or non-ASCII bytes. Job titles are built from file paths,
/// so both are reachable — fold them into a safe ASCII subset rather
/// than letting the request builder reject the whole notification. The
/// length cap keeps a pathological path out of the header block.
fn header_safe(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '?'
            }
        })
        .take(200)
        .collect()
}

/// POST a pre-formatted webhook `payload` to `url`. Public so a caller that
/// already built the body (via [`crate::format_webhook_payload`]) can
/// deliver it directly.
pub async fn send_webhook(
    client: &Client,
    url: &str,
    payload: &serde_json::Value,
) -> Result<(), ServerError> {
    send_request(url, client.post(url).json(payload)).await
}

/// Send a prepared request and fold the outcome into [`ServerError`].
/// Shared by the JSON and plain-text delivery paths so both scrub the
/// URL out of error messages the same way.
async fn send_request(url: &str, req: reqwest::RequestBuilder) -> Result<(), ServerError> {
    let resp = req.send().await.map_err(|e| {
        // Never surface `e.to_string()`: reqwest embeds the full URL, and
        // the webhook URL *is* the secret (the token lives in its path).
        // Report only the host + error category.
        let kind = if e.is_timeout() {
            "timed out"
        } else if e.is_connect() {
            "connection failed"
        } else {
            "request failed"
        };
        ServerError::Webhook(format!("{} {kind}", url_host(url)))
    })?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(ServerError::Webhook(format!(
            "{} returned HTTP {}",
            url_host(url),
            resp.status()
        )))
    }
}

/// Best-effort host for an error message, without pulling in a URL parser.
fn url_host(url: &str) -> &str {
    url.split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or(url)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::body::Bytes;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::routing::post;

    use super::*;

    type Store = Arc<Mutex<Option<serde_json::Value>>>;

    async fn capture(State(store): State<Store>, body: Bytes) -> &'static str {
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        *store.lock().unwrap() = Some(v);
        "ok"
    }

    /// Spin a loopback receiver, deliver a Slack notification, and confirm
    /// the captured JSON carries the formatted text.
    #[tokio::test]
    async fn delivers_and_captures_payload() {
        let store: Store = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route("/hook", post(capture))
            .with_state(store.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let sink = WebhookSink {
            target: WebhookTarget::Slack,
            url: format!("http://{addr}/hook"),
            pushover: None,
        };
        let event = JobNotification {
            kind: "job_completed".into(),
            title: "Done".into(),
            body: "5 files".into(),
            ok: true,
        };
        sink.deliver(&Client::new(), &event).await.expect("deliver");

        let got = store.lock().unwrap().clone().expect("captured a payload");
        let text = got.get("text").and_then(|v| v.as_str()).unwrap();
        assert!(
            text.contains("Done") && text.contains("5 files"),
            "text: {text}"
        );
        server.abort();
    }

    /// Pushover credentials from the sink config land in the body.
    #[tokio::test]
    async fn pushover_creds_are_injected() {
        let store: Store = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route("/hook", post(capture))
            .with_state(store.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let sink = WebhookSink {
            target: WebhookTarget::Pushover,
            url: format!("http://{addr}/hook"),
            pushover: Some(PushoverCreds {
                token: "app-tok".into(),
                user: "user-key".into(),
            }),
        };
        let event = JobNotification {
            kind: "job_failed".into(),
            title: "Oops".into(),
            body: "disk full".into(),
            ok: false,
        };
        sink.deliver(&Client::new(), &event).await.expect("deliver");

        let got = store.lock().unwrap().clone().unwrap();
        assert_eq!(got.get("token").and_then(|v| v.as_str()), Some("app-tok"));
        assert_eq!(got.get("user").and_then(|v| v.as_str()), Some("user-key"));
        server.abort();
    }

    /// ntfy must receive the message as a plain-text body with the
    /// title in a header. Posting the JSON envelope to a topic URL made
    /// ntfy treat the serialized object as the message text, so the
    /// device showed a raw `{"topic":...}` blob.
    #[tokio::test]
    async fn ntfy_posts_plain_text_with_headers() {
        type Raw = Arc<Mutex<Option<(String, String, String)>>>;
        async fn capture_raw(
            State(store): State<Raw>,
            headers: axum::http::HeaderMap,
            body: Bytes,
        ) -> &'static str {
            let title = headers
                .get("Title")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let priority = headers
                .get("Priority")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            *store.lock().unwrap() =
                Some((title, priority, String::from_utf8_lossy(&body).into_owned()));
            "ok"
        }

        let store: Raw = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route("/my-topic", post(capture_raw))
            .with_state(store.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let sink = WebhookSink {
            target: WebhookTarget::Ntfy,
            url: format!("http://{addr}/my-topic"),
            pushover: None,
        };
        let event = JobNotification {
            kind: "job_failed".into(),
            title: "Copy failed".into(),
            body: "disk full".into(),
            ok: false,
        };
        sink.deliver(&Client::new(), &event).await.expect("deliver");

        let (title, priority, body) = store.lock().unwrap().clone().expect("captured");
        assert_eq!(body, "disk full", "body must be the bare message");
        assert_eq!(title, "Copy failed");
        assert_eq!(priority, "high", "a failure should raise the priority");
        assert!(
            !body.starts_with("{"),
            "the body must not be a JSON envelope: {body}",
        );
        server.abort();
    }

    /// Titles reach ntfy through an HTTP header, which cannot carry
    /// control characters or non-ASCII. Job titles come from paths.
    #[test]
    fn header_safe_folds_unrepresentable_characters() {
        assert_eq!(header_safe("plain title"), "plain title");
        assert_eq!(header_safe("café"), "caf?");
        assert_eq!(
            header_safe(
                "line
break"
            ),
            "line?break"
        );
        assert_eq!(header_safe(&"x".repeat(500)).len(), 200);
    }

    /// A non-2xx response surfaces as a `Webhook` error.
    #[tokio::test]
    async fn non_2xx_is_an_error() {
        let app = Router::new().route(
            "/hook",
            post(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "boom") }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let err = send_webhook(
            &Client::new(),
            &format!("http://{addr}/hook"),
            &serde_json::json!({ "text": "x" }),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ServerError::Webhook(_)), "got {err:?}");
        server.abort();
    }
}
