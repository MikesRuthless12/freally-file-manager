//! Push-notification dispatch primitives.
//!
//! `PushTarget` is the typed wrapper around an APNs device token
//! (iOS) or an FCM registration token (Android). The runner hands a
//! `PushPayload` to a [`NotifyDispatcher`] and the dispatcher
//! issues an HTTP POST against the provider endpoint configured in
//! `MobileSettings`.
//!
//! **Provider-token signing (Phase 37 follow-up).** APNs uses
//! ES256 JWTs minted from a `.p8` team key; FCM HTTP v1 uses RS256
//! JWTs minted from a Google service-account JSON. Both signers
//! live in this module behind the [`PushSigner`] trait so the
//! [`HttpDispatcher`] can plug either in interchangeably; the
//! follow-up Tauri runner reads the signer credentials out of the
//! OS keychain and constructs the right signer for each push.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};

/// Where the desktop pushes a notification to. Variants cover the
/// two production providers (Apple, Google) plus a `Stub` flavour
/// the smoke test points at a localhost mock so we don't burn real
/// device tokens during CI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PushTarget {
    /// Apple Push Notification Service token (iOS / iPadOS).
    Apns {
        /// 64-character hex-encoded device token issued by APNs.
        token: String,
    },
    /// Firebase Cloud Messaging registration token (Android).
    Fcm {
        /// Long alphanumeric registration token issued by FCM.
        token: String,
    },
    /// Smoke-test only: POSTs the payload to an arbitrary HTTP URL
    /// so the call surface is exercised without external side
    /// effects. The runner never selects this in production code.
    #[serde(rename = "stub_endpoint")]
    StubEndpoint {
        /// Full URL the dispatcher POSTs to. Useful in tests with
        /// a localhost mock listening on a random port.
        url: String,
    },
}

/// Body the desktop wants to surface on the phone's lock screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushPayload {
    pub title: String,
    pub body: String,
    /// Optional small icon hint. iOS / Android clients map this to
    /// the appropriate native asset.
    pub icon: Option<String>,
    /// Optional deep-link URL the phone navigates to when the user
    /// taps the notification (`freally-mobile://job/<row-id>`).
    pub deep_link: Option<String>,
}

/// What the dispatcher hands back after a successful POST. Stripped
/// of any provider-specific fields so callers can record the
/// outcome generically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushReceipt {
    pub provider: String,
    pub status: u16,
}

#[derive(Debug, thiserror::Error)]
pub enum PushSendError {
    #[error("http {provider}: {source}")]
    Http {
        provider: &'static str,
        #[source]
        source: reqwest::Error,
    },
    #[error("http {provider} returned {status} (body: {body})")]
    BadStatus {
        provider: &'static str,
        status: u16,
        body: String,
    },
    #[error("sign {provider}: {reason}")]
    Sign {
        provider: &'static str,
        reason: String,
    },
}

/// Boundary the runner targets. Real APNs / FCM dispatchers
/// implement this in the Phase 37 follow-up; today the default
/// implementation is `HttpDispatcher`, an unsigned reqwest-backed
/// POST.
#[async_trait::async_trait]
pub trait NotifyDispatcher: Send + Sync {
    async fn send(
        &self,
        target: &PushTarget,
        payload: &PushPayload,
    ) -> Result<PushReceipt, PushSendError>;
}

/// Default dispatcher: POSTs the JSON payload against the target
/// URL. When an [`Arc<dyn PushSigner>`] is wired via
/// [`HttpDispatcher::with_signer`], the dispatcher attaches the
/// `Authorization: bearer <jwt>` header before sending. Without a
/// signer, requests go out unsigned — useful for the smoke test +
/// localhost mocks (`PushTarget::StubEndpoint`) but rejected by the
/// real APNs / FCM endpoints with a 401.
pub struct HttpDispatcher {
    client: reqwest::Client,
    signer: Option<Arc<dyn PushSigner>>,
}

impl HttpDispatcher {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            signer: None,
        }
    }

    /// Attach a signer that mints the per-request bearer token.
    pub fn with_signer(mut self, signer: Arc<dyn PushSigner>) -> Self {
        self.signer = Some(signer);
        self
    }

    /// Resolve the per-target URL the dispatcher hits.
    ///
    /// The FCM project comes from the signer, because it is a property
    /// of the service account being used. It used to be the literal
    /// string `freally` in the path, which is not a project anyone owns,
    /// so every Android push 404ed no matter how it was configured.
    fn url_for(
        target: &PushTarget,
        signer: Option<&Arc<dyn PushSigner>>,
    ) -> (&'static str, String) {
        match target {
            PushTarget::Apns { token } => (
                "apns",
                format!("https://api.push.apple.com/3/device/{token}"),
            ),
            PushTarget::Fcm { .. } => {
                let project = signer.and_then(|s| s.project_id()).unwrap_or_default();
                (
                    "fcm",
                    format!("https://fcm.googleapis.com/v1/projects/{project}/messages:send"),
                )
            }
            PushTarget::StubEndpoint { url } => ("stub", url.clone()),
        }
    }

    /// Build the request body in whatever envelope the provider expects.
    ///
    /// Both real providers were being sent the bare [`PushPayload`],
    /// which neither accepts: APNs requires the alert nested under an
    /// `aps` key, and FCM v1 requires a `message` object that also
    /// carries the destination token — so FCM was never told which
    /// device to deliver to. The stub target keeps the flat shape,
    /// which is what the smoke test asserts on.
    pub(crate) fn body_for(target: &PushTarget, payload: &PushPayload) -> serde_json::Value {
        match target {
            PushTarget::Apns { .. } => {
                let mut aps = serde_json::json!({
                    "alert": { "title": payload.title, "body": payload.body },
                    "sound": "default",
                });
                if let Some(icon) = &payload.icon {
                    aps["launch-image"] = serde_json::Value::String(icon.clone());
                }
                let mut root = serde_json::json!({ "aps": aps });
                // Custom keys ride alongside `aps`, which is where the
                // app reads them from on tap.
                if let Some(link) = &payload.deep_link {
                    root["deep_link"] = serde_json::Value::String(link.clone());
                }
                root
            }
            PushTarget::Fcm { token } => {
                let mut message = serde_json::json!({
                    "token": token,
                    "notification": { "title": payload.title, "body": payload.body },
                });
                let mut data = serde_json::Map::new();
                if let Some(link) = &payload.deep_link {
                    data.insert("deep_link".into(), serde_json::Value::String(link.clone()));
                }
                if let Some(icon) = &payload.icon {
                    data.insert("icon".into(), serde_json::Value::String(icon.clone()));
                }
                if !data.is_empty() {
                    message["data"] = serde_json::Value::Object(data);
                }
                serde_json::json!({ "message": message })
            }
            PushTarget::StubEndpoint { .. } => {
                serde_json::to_value(payload).unwrap_or(serde_json::Value::Null)
            }
        }
    }
}

impl Default for HttpDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl NotifyDispatcher for HttpDispatcher {
    async fn send(
        &self,
        target: &PushTarget,
        payload: &PushPayload,
    ) -> Result<PushReceipt, PushSendError> {
        let (provider, url) = Self::url_for(target, self.signer.as_ref());
        let mut req = self
            .client
            .post(&url)
            .json(&Self::body_for(target, payload));

        // APNs rejects a push with no `apns-topic`, and wants to be told
        // the push type; neither header was being sent, so every iOS
        // push came back 400 regardless of how the key was configured.
        if matches!(target, PushTarget::Apns { .. }) {
            req = req.header("apns-push-type", "alert");
            if let Some(topic) = self.signer.as_ref().and_then(|s| s.bundle_id()) {
                req = req.header("apns-topic", topic);
            }
        }

        if let Some(signer) = &self.signer {
            let token = signer
                .bearer(&self.client)
                .await
                .map_err(|e| PushSendError::Sign {
                    provider,
                    reason: e,
                })?;
            req = req.bearer_auth(token);
        }
        let resp = req
            .send()
            .await
            .map_err(|source| PushSendError::Http { provider, source })?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(PushSendError::BadStatus {
                provider,
                status,
                body,
            });
        }
        Ok(PushReceipt {
            provider: provider.to_string(),
            status,
        })
    }
}

// ---------------------------------------------------------------------
// Push signers (APNs ES256 + FCM RS256)
// ---------------------------------------------------------------------

/// Signs a per-request bearer token. The dispatcher wires this in
/// behind a `Send + Sync` `Arc` so a single signer is shared across
/// every concurrent push.
#[async_trait::async_trait]
pub trait PushSigner: Send + Sync + std::fmt::Debug {
    /// The value for the `Authorization: bearer` header.
    ///
    /// Async because FCM needs a round trip: a service-account JWT is
    /// not itself an FCM credential, it is what you exchange at Google's
    /// token endpoint for an OAuth2 access token. Sending the JWT
    /// straight through — which is what this did — is rejected.
    /// Borrows the caller's client so the exchange reuses the same
    /// connection pool and timeout.
    async fn bearer(&self, client: &reqwest::Client) -> Result<String, String>;

    /// FCM: the project the service account belongs to, which forms
    /// part of the send URL. `None` for providers where it is not a
    /// concept.
    fn project_id(&self) -> Option<&str> {
        None
    }

    /// APNs: the app bundle identifier, sent as the mandatory
    /// `apns-topic` header.
    fn bundle_id(&self) -> Option<&str> {
        None
    }
}

/// APNs token-based authentication.
///
/// Apple issues a `.p8` PEM-encoded ECDSA P-256 private key alongside
/// a 10-character `key_id` and a 10-character `team_id`. The runner
/// stores the PEM in the OS keychain (under
/// `freally-mobile/apns_p8`), reads it back at startup, and hands
/// the bytes to [`ApnsSigner::new`]. Each `sign_for` call mints a
/// fresh ES256 JWT with `iat = now`, `exp = now + 1h`, `iss = team_id`,
/// and `kid = key_id` in the header — the format documented at
/// <https://developer.apple.com/documentation/usernotifications/establishing_a_token-based_connection_to_apns>.
#[derive(Debug, Clone)]
pub struct ApnsSigner {
    pub team_id: String,
    pub key_id: String,
    /// The app bundle identifier. APNs requires it on every request as
    /// the `apns-topic` header — a push without it is refused before
    /// the token is even considered.
    pub bundle_id: String,
    /// PEM-encoded ECDSA P-256 private key (`-----BEGIN PRIVATE KEY-----`
    /// … `-----END PRIVATE KEY-----`). Kept as bytes so the signer
    /// can be created from the keychain without round-tripping
    /// through `String`.
    pub p8_pem: Vec<u8>,
}

impl ApnsSigner {
    /// Build a signer from the team + key identifiers and the raw
    /// PEM bytes of the p8 file. Returns an error if the PEM does
    /// not parse as an ECDSA P-256 key.
    pub fn new(
        team_id: impl Into<String>,
        key_id: impl Into<String>,
        bundle_id: impl Into<String>,
        p8_pem: Vec<u8>,
    ) -> Result<Self, String> {
        // Validate the key parses now so a misconfigured signer fails
        // at construction rather than at first push.
        EncodingKey::from_ec_pem(&p8_pem).map_err(|e| format!("apns p8: {e}"))?;
        let bundle_id = bundle_id.into();
        if bundle_id.trim().is_empty() {
            return Err("apns bundle id (apns-topic) is required".into());
        }
        Ok(Self {
            team_id: team_id.into(),
            key_id: key_id.into(),
            bundle_id,
            p8_pem,
        })
    }
}

#[derive(Debug, Serialize)]
struct ApnsClaims<'a> {
    iss: &'a str,
    iat: u64,
    exp: u64,
}

#[async_trait::async_trait]
impl PushSigner for ApnsSigner {
    fn bundle_id(&self) -> Option<&str> {
        Some(&self.bundle_id)
    }

    /// APNs takes the signed JWT directly — no exchange — so this is
    /// async only to satisfy the shared trait.
    async fn bearer(&self, _client: &reqwest::Client) -> Result<String, String> {
        let key = EncodingKey::from_ec_pem(&self.p8_pem).map_err(|e| format!("apns p8: {e}"))?;
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(self.key_id.clone());
        let now = unix_now();
        let claims = ApnsClaims {
            iss: &self.team_id,
            iat: now,
            exp: now + 60 * 60, // 1 hour — APNs caps tokens at ~1h.
        };
        jsonwebtoken::encode(&header, &claims, &key).map_err(|e| format!("apns sign: {e}"))
    }
}

/// FCM HTTP v1 token-based authentication via a Google service-
/// account JSON. The runner stores the JSON in the keychain (under
/// `freally-mobile/fcm_service_account`); the signer parses out the
/// PEM private key, the client_email, and the token_uri at
/// construction time. Each `sign_for` call mints a fresh RS256 JWT
/// scoped for `https://www.googleapis.com/auth/firebase.messaging`.
#[derive(Debug, Clone)]
pub struct FcmSigner {
    pub client_email: String,
    pub project_id: String,
    /// The access token most recently obtained, and when it expires.
    /// Google issues them with an hour of life; re-exchanging on every
    /// push would add a round trip to Google in front of every
    /// notification.
    cached: Arc<std::sync::Mutex<Option<CachedToken>>>,
    /// PEM-encoded RSA private key, extracted from the service
    /// account JSON's `private_key` field at construction time.
    pub rsa_pem: Vec<u8>,
}

impl FcmSigner {
    /// Build a signer from a Google service-account JSON blob (raw
    /// bytes, exactly as the file ships). Returns an error if the
    /// JSON is malformed or the key does not parse as RSA.
    pub fn from_service_account_json(json_bytes: &[u8]) -> Result<Self, String> {
        #[derive(Deserialize)]
        struct ServiceAccount {
            project_id: String,
            client_email: String,
            private_key: String,
        }
        let sa: ServiceAccount =
            serde_json::from_slice(json_bytes).map_err(|e| format!("fcm json: {e}"))?;
        let pem_bytes = sa.private_key.into_bytes();
        EncodingKey::from_rsa_pem(&pem_bytes).map_err(|e| format!("fcm rsa: {e}"))?;
        Ok(Self {
            client_email: sa.client_email,
            project_id: sa.project_id,
            rsa_pem: pem_bytes,
            cached: Arc::new(std::sync::Mutex::new(None)),
        })
    }
}

#[derive(Debug, Serialize)]
struct FcmClaims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    iat: u64,
    exp: u64,
}

/// An OAuth2 access token and the moment it stops being usable.
#[derive(Debug, Clone)]
struct CachedToken {
    value: String,
    expires_at: u64,
}

impl FcmSigner {
    /// Mint the self-signed assertion Google exchanges for an access
    /// token. `aud` is the token endpoint — this JWT is not the
    /// credential, it is the thing traded for one.
    fn assertion(&self) -> Result<String, String> {
        let key = EncodingKey::from_rsa_pem(&self.rsa_pem).map_err(|e| format!("fcm rsa: {e}"))?;
        let header = Header::new(Algorithm::RS256);
        let now = unix_now();
        let claims = FcmClaims {
            iss: &self.client_email,
            scope: "https://www.googleapis.com/auth/firebase.messaging",
            aud: FCM_TOKEN_ENDPOINT,
            iat: now,
            exp: now + 60 * 60,
        };
        jsonwebtoken::encode(&header, &claims, &key).map_err(|e| format!("fcm sign: {e}"))
    }
}

/// Google's OAuth2 token endpoint.
const FCM_TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";

/// Re-exchange this many seconds before the token actually lapses, so a
/// push in flight when it expires does not fail on a technicality.
const TOKEN_REFRESH_SKEW_SECS: u64 = 60;

#[async_trait::async_trait]
impl PushSigner for FcmSigner {
    fn project_id(&self) -> Option<&str> {
        Some(&self.project_id)
    }

    async fn bearer(&self, client: &reqwest::Client) -> Result<String, String> {
        let now = unix_now();
        if let Ok(guard) = self.cached.lock() {
            if let Some(tok) = guard.as_ref() {
                if tok.expires_at > now + TOKEN_REFRESH_SKEW_SECS {
                    return Ok(tok.value.clone());
                }
            }
        }

        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: String,
            #[serde(default)]
            expires_in: u64,
        }

        let assertion = self.assertion()?;
        let resp = client
            .post(FCM_TOKEN_ENDPOINT)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .await
            .map_err(|e| format!("fcm token exchange: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            // The body echoes the assertion back on some errors, so it
            // is not safe to surface verbatim.
            return Err(format!("fcm token exchange returned status {status}"));
        }
        let parsed: TokenResponse = resp
            .json()
            .await
            .map_err(|e| format!("fcm token response: {e}"))?;

        if let Ok(mut guard) = self.cached.lock() {
            *guard = Some(CachedToken {
                value: parsed.access_token.clone(),
                // Treat a missing `expires_in` as short-lived rather
                // than eternal; the cost of guessing wrong that way is
                // one extra exchange.
                expires_at: now
                    + if parsed.expires_in == 0 {
                        300
                    } else {
                        parsed.expires_in
                    },
            });
        }
        Ok(parsed.access_token)
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{DecodingKey, Validation};

    #[test]
    fn push_target_round_trips_through_serde() {
        let t = PushTarget::Apns {
            token: "ab".repeat(32),
        };
        let s = serde_json::to_string(&t).unwrap();
        let back: PushTarget = serde_json::from_str(&s).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn url_for_smoke_endpoints() {
        let stub = PushTarget::StubEndpoint {
            url: "http://127.0.0.1:9000/push".into(),
        };
        let (provider, url) = HttpDispatcher::url_for(&stub, None);
        assert_eq!(provider, "stub");
        assert!(url.starts_with("http://"));
    }

    /// The FCM send URL names the project the service account belongs
    /// to. It used to be the literal `freally`, which is nobody's
    /// project, so every Android push 404ed however it was configured.
    #[test]
    fn fcm_url_names_the_service_account_project() {
        let (priv_pem, _pub) = make_rsa_pem_pair();
        let json = serde_json::json!({
            "type": "service_account",
            "project_id": "my-real-project",
            "client_email": "robot@my-real-project.iam.gserviceaccount.com",
            "private_key": String::from_utf8(priv_pem).unwrap(),
        });
        let signer: Arc<dyn PushSigner> =
            Arc::new(FcmSigner::from_service_account_json(json.to_string().as_bytes()).unwrap());
        let target = PushTarget::Fcm {
            token: "device-token".into(),
        };
        let (provider, url) = HttpDispatcher::url_for(&target, Some(&signer));
        assert_eq!(provider, "fcm");
        assert_eq!(
            url,
            "https://fcm.googleapis.com/v1/projects/my-real-project/messages:send"
        );
    }

    /// FCM v1 wants the destination token inside a `message` object.
    /// The bare payload was going out instead, so FCM was never told
    /// which device to deliver to.
    #[test]
    fn fcm_body_carries_the_device_token() {
        let target = PushTarget::Fcm {
            token: "device-token-abc".into(),
        };
        let payload = PushPayload {
            title: "Copy finished".into(),
            body: "412 files".into(),
            icon: None,
            deep_link: Some("freally-mobile://job/7".into()),
        };
        let body = HttpDispatcher::body_for(&target, &payload);
        assert_eq!(body["message"]["token"], "device-token-abc");
        assert_eq!(body["message"]["notification"]["title"], "Copy finished");
        assert_eq!(body["message"]["notification"]["body"], "412 files");
        assert_eq!(
            body["message"]["data"]["deep_link"],
            "freally-mobile://job/7"
        );
    }

    /// APNs reads the alert from an `aps` envelope; a flat object is
    /// not a notification to it.
    #[test]
    fn apns_body_uses_the_aps_envelope() {
        let target = PushTarget::Apns {
            token: "deadbeef".into(),
        };
        let payload = PushPayload {
            title: "Copy finished".into(),
            body: "412 files".into(),
            icon: None,
            deep_link: Some("freally-mobile://job/7".into()),
        };
        let body = HttpDispatcher::body_for(&target, &payload);
        assert_eq!(body["aps"]["alert"]["title"], "Copy finished");
        assert_eq!(body["aps"]["alert"]["body"], "412 files");
        // Custom keys ride beside `aps`, never inside it.
        assert_eq!(body["deep_link"], "freally-mobile://job/7");
        assert!(body["aps"]["deep_link"].is_null());
    }

    /// The stub target keeps the flat shape the smoke test asserts on.
    #[test]
    fn stub_body_stays_flat() {
        let target = PushTarget::StubEndpoint {
            url: "http://127.0.0.1:1/p".into(),
        };
        let payload = PushPayload {
            title: "t".into(),
            body: "b".into(),
            icon: None,
            deep_link: None,
        };
        let body = HttpDispatcher::body_for(&target, &payload);
        assert_eq!(body["title"], "t");
        assert_eq!(body["body"], "b");
    }

    /// Mint a test ECDSA P-256 keypair as a PEM PKCS#8 + matching
    /// public-key PEM. Reuses rcgen's primitives (already a dep for
    /// `EphemeralCert`) so the test doesn't grow the dep tree.
    fn make_p256_pem_pair() -> (Vec<u8>, Vec<u8>) {
        let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let priv_pem = key_pair.serialize_pem();
        let pub_pem = key_pair.public_key_pem();
        (priv_pem.into_bytes(), pub_pem.into_bytes())
    }

    fn make_rsa_pem_pair() -> (Vec<u8>, Vec<u8>) {
        // rcgen 0.14 supports PKCS_RSA_SHA256 only when paired with a
        // user-supplied RSA key (rcgen itself doesn't ship an RSA
        // generator). For the FCM signer test we generate an RSA key
        // via the `rsa` crate (already in the workspace transitively
        // via `age`) and serialize to PKCS#8 PEM.
        use rsa::pkcs1::EncodeRsaPublicKey;
        use rsa::pkcs8::{EncodePrivateKey, LineEnding};
        use rsa::{RsaPrivateKey, RsaPublicKey};
        let mut rng = rand_compat::SystemRng;
        let priv_key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let pub_key = RsaPublicKey::from(&priv_key);
        let priv_pem = priv_key.to_pkcs8_pem(LineEnding::LF).unwrap().to_string();
        let pub_pem = pub_key.to_pkcs1_pem(LineEnding::LF).unwrap();
        (priv_pem.into_bytes(), pub_pem.into_bytes())
    }

    /// Minimal CryptoRng + RngCore shim so `rsa::RsaPrivateKey::new`
    /// can pull entropy from `getrandom` without dragging the full
    /// `rand` ecosystem in as a dev-dep.
    mod rand_compat {
        pub struct SystemRng;
        impl rand_core::RngCore for SystemRng {
            fn next_u32(&mut self) -> u32 {
                let mut buf = [0u8; 4];
                getrandom::fill(&mut buf).unwrap();
                u32::from_le_bytes(buf)
            }
            fn next_u64(&mut self) -> u64 {
                let mut buf = [0u8; 8];
                getrandom::fill(&mut buf).unwrap();
                u64::from_le_bytes(buf)
            }
            fn fill_bytes(&mut self, dst: &mut [u8]) {
                getrandom::fill(dst).unwrap();
            }
            fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), rand_core::Error> {
                getrandom::fill(dst)
                    .map_err(|e| rand_core::Error::new(std::io::Error::other(e.to_string())))
            }
        }
        impl rand_core::CryptoRng for SystemRng {}
    }

    #[tokio::test]
    async fn apns_signer_mints_es256_jwt_with_team_iss_and_kid() {
        let (priv_pem, pub_pem) = make_p256_pem_pair();
        let signer = ApnsSigner::new("ABCDE12345", "KEY1234567", "com.freally.app", priv_pem)
            .expect("apns signer");
        assert_eq!(signer.bundle_id(), Some("com.freally.app"));
        let token = signer.bearer(&reqwest::Client::new()).await.expect("sign");
        assert_eq!(token.matches('.').count(), 2, "expected three JWT segments");

        let decoding = DecodingKey::from_ec_pem(&pub_pem).expect("decoding key");
        let mut v = Validation::new(Algorithm::ES256);
        v.validate_exp = true;
        v.validate_aud = false;
        v.required_spec_claims.clear();
        let data =
            jsonwebtoken::decode::<serde_json::Value>(&token, &decoding, &v).expect("verify");
        assert_eq!(data.claims["iss"], "ABCDE12345");
        let kid = data.header.kid.as_deref().unwrap_or_default();
        assert_eq!(kid, "KEY1234567");
    }

    #[test]
    fn fcm_signer_parses_service_account_and_signs_rs256() {
        let (priv_pem, pub_pem) = make_rsa_pem_pair();
        let priv_pem_str = String::from_utf8(priv_pem).unwrap();
        let json = serde_json::json!({
            "type": "service_account",
            "project_id": "freally-test",
            "client_email": "robot@freally-test.iam.gserviceaccount.com",
            "private_key": priv_pem_str,
        });
        let signer =
            FcmSigner::from_service_account_json(json.to_string().as_bytes()).expect("fcm signer");
        assert_eq!(signer.project_id, "freally-test");
        assert_eq!(
            signer.client_email,
            "robot@freally-test.iam.gserviceaccount.com"
        );

        // `bearer` would exchange this at Google; the assertion is the
        // part that can be verified without a network.
        let token = signer.assertion().expect("sign");
        let decoding = DecodingKey::from_rsa_pem(&pub_pem).expect("decoding key");
        let mut v = Validation::new(Algorithm::RS256);
        v.validate_exp = true;
        v.validate_aud = false;
        v.required_spec_claims.clear();
        let data =
            jsonwebtoken::decode::<serde_json::Value>(&token, &decoding, &v).expect("verify");
        assert_eq!(
            data.claims["iss"],
            "robot@freally-test.iam.gserviceaccount.com"
        );
        assert_eq!(
            data.claims["scope"],
            "https://www.googleapis.com/auth/firebase.messaging"
        );
        // The assertion is addressed to the token endpoint, because it
        // is exchanged there rather than presented to FCM. Sending it
        // straight to FCM as a bearer token — which is what used to
        // happen — is refused.
        assert_eq!(data.claims["aud"], "https://oauth2.googleapis.com/token");
    }

    #[test]
    fn apns_signer_rejects_malformed_p8() {
        let err = ApnsSigner::new("X", "Y", "com.freally.app", b"not a pem".to_vec()).unwrap_err();
        assert!(err.contains("apns p8"), "{err}");
    }

    #[test]
    fn fcm_signer_rejects_non_json_bytes() {
        let err = FcmSigner::from_service_account_json(b"{not json").unwrap_err();
        assert!(err.contains("fcm json"), "{err}");
    }
}
