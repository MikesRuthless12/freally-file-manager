//! Phase 32f — SFTP via russh + russh-sftp.
//!
//! OpenDAL's `services-sftp` pulls in the `openssh` 0.11 crate,
//! which fails to compile on Windows (blanket `TryFrom` conflict
//! upstream). Phase 32f lights SFTP anyway via a custom
//! [`CopyTarget`] impl backed by [`russh`] (pure-Rust SSH client)
//! and [`russh_sftp`] (client-side SFTP subsystem). Both crates are
//! MIT-licensed and cross-compile cleanly on Windows/macOS/Linux.
//!
//! # Authentication
//!
//! Three modes supported, selected by the first line of the
//! keychain-stored secret blob:
//!
//! - **Password**: no prefix line OR first line is `PASS`. The
//!   remainder is the raw password.
//! - **Unencrypted private key**: first line is `KEY`. The
//!   remainder is the OpenSSH-format private key.
//! - **Encrypted private key** (Phase 32g): first line is `KEY_ENC`,
//!   second line is the passphrase, remaining lines are the
//!   OpenSSH-format private key body. The passphrase is used once
//!   at connect time and never written to disk — it lives in the
//!   keychain entry alongside the key material.
//!
//! # Host verification
//!
//! Phase 32h implements `known_hosts` pinning. When
//! [`SftpConfig::known_hosts_path`] is non-empty, the handshake
//! compares the server's public key against every entry matching the
//! configured `(host, port)` — one per key type — and a missing entry
//! or a mismatch rejects the connection.
//!
//! When the field is empty the handshake **fails closed**. It does
//! not fall back to trust-on-first-use, which is what this used to
//! say: accepting an unknown key is the MITM the check exists to
//! stop.
//!
//! # Scope
//!
//! Phase 32f ships:
//! - [`SftpTarget`] — `CopyTarget` impl with `put` / `get` /
//!   `list` / `stat` / `delete`.
//! - Connection-per-request model (opens a fresh SSH session for
//!   each call). Phase 32g adds pooling.
//! - Minimal config validation (host/user required; port defaults
//!   to 22).
//!
//! Deferred to Phase 32g:
//! - Connection pooling via `Arc<Mutex<Session>>` on the target.
//! - Encrypted-key passphrase handling.
//! - `known_hosts` pinning.
//! - Streaming writer (big-file uploads land in one buffer today).

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use russh::client::{self, Handle};
use russh::keys::PrivateKeyWithHashAlg;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::OpenFlags;
use tokio::sync::Mutex as AsyncMutex;

use crate::backend::SftpConfig;
use crate::error::BackendError;
use crate::target::{CopyTarget, EntryMeta};

/// Phase 32h — host-key verifier. `expected_sha256_hex` holds the
/// hex-encoded SHA-256 fingerprint of *every* key pinned for this
/// host in `known_hosts`, one per key type. An empty set means the
/// host is not pinned, and the handshake fails closed.
///
/// This was a single `Option<String>` holding whichever entry came
/// first in the file. A `known_hosts` seeded normally by OpenSSH
/// carries three lines per host (ssh-rsa, ecdsa-sha2-nistp256,
/// ssh-ed25519), and the server picks the type during negotiation —
/// so a correctly-pinned host was refused whenever the negotiated
/// type was not the one listed first.
struct HostKeyVerifier {
    expected_sha256_hex: Vec<String>,
}

impl client::Handler for HostKeyVerifier {
    type Error = russh::Error;

    // russh 0.60 uses an `impl Future`-returning method rather than
    // `async fn` for trait dispatch.
    #[allow(clippy::manual_async_fn)]
    fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> impl std::future::Future<Output = Result<bool, Self::Error>> + Send {
        // Capture the expected fingerprint by value for the
        // returned future; the trait's lifetime bound on
        // `server_public_key` means we compute eagerly here.
        let expected = self.expected_sha256_hex.clone();
        let actual_hex = sha256_fingerprint_hex(server_public_key);
        async move {
            // Fail closed on an empty set. This used to `Ok(true)` —
            // accept any host key — whenever no expected fingerprint
            // was configured, which is exactly the MITM the host-key
            // check exists to stop.
            //
            // Any pinned key for the host is acceptable: every line the
            // user keeps for that host is one they already trust, and
            // the server chooses which type to present.
            Ok(expected
                .iter()
                .any(|exp| actual_hex.eq_ignore_ascii_case(exp)))
        }
    }
}

/// True when an SFTP error means "this path does not exist", as
/// opposed to "I could not tell you whether it does".
fn is_absent(e: &russh_sftp::client::error::Error) -> bool {
    use russh_sftp::protocol::StatusCode;
    matches!(
        e,
        russh_sftp::client::error::Error::Status(s)
            if s.status_code == StatusCode::NoSuchFile
    )
}

/// Phase 32h — pool state. `None` = never connected / torn down.
type PooledSession = Option<(Handle<HostKeyVerifier>, SftpSession)>;

/// Phase 32i — compare a hashed OpenSSH `|1|<salt>|<hmac>` entry
/// against each candidate host form (plain + bracketed-with-port).
/// OpenSSH's `HashKnownHosts yes` writes entries as
/// `|1|<base64-salt>|<base64-HMAC-SHA1(salt, hostname)>`. Matching
/// requires recomputing the HMAC for each candidate form and
/// base64-comparing the result.
fn hashed_hostfield_matches(hosts_field: &str, search_plain: &str, search_bracketed: &str) -> bool {
    use base64::Engine;
    use hmac::{Hmac, Mac};
    use sha1::Sha1;

    // Strip leading `|1|`; split the rest on `|`.
    let rest = &hosts_field[3..];
    let (salt_b64, hmac_b64) = match rest.split_once('|') {
        Some((s, h)) => (s, h),
        None => return false,
    };
    let Ok(salt) = base64::engine::general_purpose::STANDARD.decode(salt_b64) else {
        return false;
    };
    let Ok(expected_hmac) = base64::engine::general_purpose::STANDARD.decode(hmac_b64) else {
        return false;
    };

    for candidate in [search_plain, search_bracketed] {
        let Ok(mut mac) = <Hmac<Sha1> as Mac>::new_from_slice(&salt) else {
            continue;
        };
        mac.update(candidate.as_bytes());
        let actual = mac.finalize().into_bytes();
        if actual.as_slice() == expected_hmac.as_slice() {
            return true;
        }
    }
    false
}

fn sha256_fingerprint_hex(key: &russh::keys::ssh_key::PublicKey) -> String {
    use sha2::Digest;
    let Ok(bytes) = key.to_bytes() else {
        return String::new();
    };
    let mut hasher = sha2::Sha256::new();
    hasher.update(&bytes);
    hex::encode(hasher.finalize())
}

/// Parse an OpenSSH `known_hosts` file and return the server key's
/// SHA-256 hex fingerprint of every key pinned for the given
/// `(host, port)` — one per key type present. Returns an empty `Vec`
/// when no entry matches. Errors on unreadable / malformed content.
///
/// All matching lines are returned, not just the first: OpenSSH seeds
/// a host with one line per key type, and which one the server
/// presents is decided during negotiation.
///
/// Supports three entry forms:
/// - Plain `host key-type base64-key` (default port 22).
/// - `[host]:port key-type base64-key` for non-22 ports.
/// - Hashed `|1|salt|hmac key-type base64-key` entries match the
///   plaintext `host:port` after HMAC-SHA1 (OpenSSH's
///   `HashKnownHosts yes` format). Phase 32h treats hashed
///   entries as "unknown" and falls through — matching hashes
///   requires HMAC-SHA1 and the live host string, which we'd
///   need to accept on the callback's `&mut self` side.
pub(crate) fn expected_host_key_fingerprints(
    known_hosts_path: &std::path::Path,
    host: &str,
    port: u16,
) -> std::io::Result<Vec<String>> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(known_hosts_path)?;
    let reader = BufReader::new(file);
    let search_plain = host.to_ascii_lowercase();
    let search_bracketed = format!("[{}]:{port}", host.to_ascii_lowercase());
    let mut found: Vec<String> = Vec::new();
    for line in reader.lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(hosts_field) = parts.next() else {
            continue;
        };
        let Some(_key_type) = parts.next() else {
            continue;
        };
        let Some(key_b64) = parts.next() else {
            continue;
        };
        // Phase 32i — hashed entries: `|1|<base64-salt>|<base64-hmac>`.
        // OpenSSH computes hmac = HMAC-SHA1(salt, hostname).
        // Match by recomputing the hmac for each candidate form
        // of the target host (plain + bracketed-port).
        let matches = if hosts_field.starts_with("|1|") {
            hashed_hostfield_matches(hosts_field, &search_plain, &search_bracketed)
        } else {
            hosts_field.split(',').any(|h| {
                let h = h.to_ascii_lowercase();
                h == search_plain || h == search_bracketed
            })
        };
        if !matches {
            continue;
        }
        // Decode the base64-encoded SSH wire key + hash it.
        use base64::Engine;
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(key_b64) else {
            continue;
        };
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        hasher.update(&bytes);
        found.push(hex::encode(hasher.finalize()));
    }
    Ok(found)
}

/// SFTP backend. Construct with [`SftpTarget::new`]; call through
/// the [`CopyTarget`] trait for uniform dispatch.
///
/// Phase 32h adds connection pooling: the target caches at most
/// one live `(Handle, SftpSession)` pair behind an async mutex;
/// concurrent `put` / `get` calls serialize through it but avoid
/// re-handshaking per request. A session becomes stale when its
/// underlying SSH channel drops — the next call detects the error
/// and transparently re-handshakes.
pub struct SftpTarget {
    name: String,
    config: SftpConfig,
    /// Bundled auth blob from the keychain. First line is either
    /// `PASS`, `KEY`, or `KEY_ENC`; rest is the password or
    /// OpenSSH-format private key body.
    secret: String,
    /// Phase 32h — pooled session. `None` until the first `put` /
    /// `get`. The mutex ensures only one handshake runs at a time;
    /// the `Option` distinguishes "never connected" from
    /// "connection known-broken" (clear on error + re-handshake
    /// lazily).
    pool: Arc<AsyncMutex<PooledSession>>,
}

impl std::fmt::Debug for SftpTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SftpTarget")
            .field("name", &self.name)
            .field("host", &self.config.host)
            .field("port", &self.config.port)
            .field("username", &self.config.username)
            .field(
                "known_hosts_pinned",
                &(!self.config.known_hosts_path.is_empty()),
            )
            .finish()
    }
}

impl SftpTarget {
    pub fn new(name: impl Into<String>, config: SftpConfig, secret: String) -> Self {
        Self {
            name: name.into(),
            config,
            secret,
            pool: Arc::new(AsyncMutex::new(None)),
        }
    }

    /// Phase 32h — obtain the live SFTP session, (re)handshaking
    /// transparently when it's been dropped. Callers hold the
    /// returned mutex guard for the duration of their SFTP
    /// operation so Rust's borrow checker enforces single-use.
    async fn with_session<F, Fut, T>(&self, op: F) -> Result<T, BackendError>
    where
        F: FnOnce(SftpSession) -> Fut,
        Fut: std::future::Future<Output = Result<(T, SftpSession), BackendError>>,
    {
        let mut guard = self.pool.lock().await;
        if guard.is_none() {
            *guard = Some(self.open_session().await?);
        }
        // Detach the session from the guard so the closure owns it
        // during the operation; we re-insert at the end.
        let (handle, sftp) = guard.take().expect("session just inserted");
        match op(sftp).await {
            Ok((value, sftp)) => {
                *guard = Some((handle, sftp));
                Ok(value)
            }
            Err(e) => {
                // Drop the handle on error so the next call
                // re-handshakes. Keep `handle` alive until here so
                // the session's cleanup runs.
                drop(handle);
                Err(e)
            }
        }
    }

    /// Build an authenticated SSH session + SFTP subsystem handle.
    async fn open_session(&self) -> Result<(Handle<HostKeyVerifier>, SftpSession), BackendError> {
        if self.config.host.is_empty() || self.config.username.is_empty() {
            return Err(BackendError::InvalidConfig(
                "sftp requires host + username".into(),
            ));
        }
        let addr = format!(
            "{}:{}",
            self.config.host,
            if self.config.port == 0 {
                22
            } else {
                self.config.port
            }
        );
        // Phase 32h — build the verifier. If `known_hosts_path` is
        // set, lift every pinned fingerprint for this (host, port).
        // An unset path yields an empty set, which fails closed.
        let expected_fp = if !self.config.known_hosts_path.is_empty() {
            expected_host_key_fingerprints(
                std::path::Path::new(&self.config.known_hosts_path),
                &self.config.host,
                if self.config.port == 0 {
                    22
                } else {
                    self.config.port
                },
            )
            .map_err(|e| BackendError::InvalidConfig(format!("known_hosts read: {e}")))?
        } else {
            Vec::new()
        };
        if self.config.known_hosts_path.is_empty() {
            // Explicit no-pin mode — an empty set, which the verifier
            // rejects. Fail closed rather than trust on first use.
        } else if expected_fp.is_empty() {
            return Err(BackendError::InvalidConfig(format!(
                "sftp known_hosts has no entry for {}:{}",
                self.config.host, self.config.port
            )));
        }
        let verifier = HostKeyVerifier {
            expected_sha256_hex: expected_fp,
        };

        let client_config = Arc::new(client::Config::default());
        let mut session = client::connect(client_config, addr.as_str(), verifier)
            .await
            .map_err(|e| BackendError::InvalidConfig(format!("sftp connect: {e}")))?;

        let auth_ok = if let Some(body) = self.secret.strip_prefix("KEY_ENC\n") {
            // Phase 32g — encrypted private key. First line after
            // the prefix is the passphrase; rest is the
            // OpenSSH-format key body.
            let mut lines = body.splitn(2, '\n');
            let passphrase = lines.next().ok_or_else(|| {
                BackendError::InvalidConfig("sftp KEY_ENC missing passphrase".into())
            })?;
            let key_body = lines.next().ok_or_else(|| {
                BackendError::InvalidConfig("sftp KEY_ENC missing key body".into())
            })?;
            let key = russh::keys::decode_secret_key(key_body, Some(passphrase))
                .map_err(|e| BackendError::InvalidConfig(format!("sftp key decrypt: {e}")))?;
            let key_with_hash = PrivateKeyWithHashAlg::new(Arc::new(key), None);
            session
                .authenticate_publickey(&self.config.username, key_with_hash)
                .await
                .map_err(|e| BackendError::InvalidConfig(format!("sftp auth: {e}")))?
                .success()
        } else if let Some(body) = self.secret.strip_prefix("KEY\n") {
            // Unencrypted private-key auth.
            let key = russh::keys::decode_secret_key(body, None)
                .map_err(|e| BackendError::InvalidConfig(format!("sftp key parse: {e}")))?;
            let key_with_hash = PrivateKeyWithHashAlg::new(Arc::new(key), None);
            session
                .authenticate_publickey(&self.config.username, key_with_hash)
                .await
                .map_err(|e| BackendError::InvalidConfig(format!("sftp auth: {e}")))?
                .success()
        } else {
            // Password auth (default — no KEY_ENC/KEY prefix, or
            // explicit PASS prefix). Normalise CRLF → LF before
            // strip_prefix so a Windows wizard producing
            // `PASS\r\nhunter2` doesn't slip the literal `PASS\r`
            // prefix and a trailing `\n` into the password (the
            // strip would miss `PASS\n` and the server would fail
            // auth with a misleading error). Also trim trailing
            // CR/LF/whitespace so a copy-paste artefact doesn't
            // turn into part of the credential.
            let normalized = self.secret.replace("\r\n", "\n").replace('\r', "\n");
            let pw_owned = normalized
                .strip_prefix("PASS\n")
                .map(|s| s.to_string())
                .unwrap_or(normalized);
            let pw = pw_owned.trim_end_matches(['\n', '\r', ' ', '\t']);
            session
                .authenticate_password(&self.config.username, pw)
                .await
                .map_err(|e| BackendError::InvalidConfig(format!("sftp auth: {e}")))?
                .success()
        };
        if !auth_ok {
            return Err(BackendError::InvalidConfig(
                "sftp authentication failed".into(),
            ));
        }

        // Open the SFTP subsystem.
        let channel = session
            .channel_open_session()
            .await
            .map_err(|e| BackendError::InvalidConfig(format!("sftp channel: {e}")))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| BackendError::InvalidConfig(format!("sftp subsystem: {e}")))?;
        let sftp = SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| BackendError::InvalidConfig(format!("sftp handshake: {e}")))?;

        Ok((session, sftp))
    }

    /// Resolve a per-target key into the full remote path, honoring
    /// `SftpConfig::root` as a prefix. Returned paths always start
    /// with `/` — SFTP is a chroot-like protocol in practice.
    fn resolve(&self, key: &str) -> String {
        let trimmed = key.trim_start_matches('/');
        if self.config.root.is_empty() {
            format!("/{trimmed}")
        } else {
            let root = self.config.root.trim_end_matches('/');
            format!("{root}/{trimmed}")
        }
    }
}

#[async_trait]
impl CopyTarget for SftpTarget {
    fn name(&self) -> &str {
        &self.name
    }

    // No OpenDAL operator here — falls through to the `CopyTarget`
    // default `None`, which keeps the streaming-writer fast-path
    // in `FreallyCloudSink` off until Phase 32g adds an
    // SFTP-native streaming writer.

    async fn put(&self, path: &str, data: Bytes) -> Result<(), BackendError> {
        use tokio::io::AsyncWriteExt;
        let remote = self.resolve(path);
        self.with_session(|sftp| async move {
            if let Some(parent) = remote.rsplit_once('/').map(|(p, _)| p)
                && !parent.is_empty()
            {
                let _ = sftp.create_dir(parent).await;
            }
            let mut file = sftp
                .open_with_flags(
                    &remote,
                    OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE,
                )
                .await
                .map_err(|e| BackendError::InvalidConfig(format!("sftp open write: {e}")))?;
            file.write_all(&data)
                .await
                .map_err(|e| BackendError::InvalidConfig(format!("sftp write: {e}")))?;
            file.shutdown()
                .await
                .map_err(|e| BackendError::InvalidConfig(format!("sftp close: {e}")))?;
            Ok(((), sftp))
        })
        .await
    }

    async fn get(&self, path: &str) -> Result<Bytes, BackendError> {
        use tokio::io::AsyncReadExt;
        let remote = self.resolve(path);
        self.with_session(|sftp| async move {
            let mut file = sftp
                .open(&remote)
                .await
                .map_err(|e| BackendError::InvalidConfig(format!("sftp open read: {e}")))?;
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)
                .await
                .map_err(|e| BackendError::InvalidConfig(format!("sftp read: {e}")))?;
            Ok((Bytes::from(buf), sftp))
        })
        .await
    }

    async fn list(&self, prefix: &str) -> Result<Vec<EntryMeta>, BackendError> {
        let remote = self.resolve(prefix);
        let remote_owned = remote.clone();
        self.with_session(|sftp| async move {
            let entries = sftp
                .read_dir(&remote_owned)
                .await
                .map_err(|e| BackendError::InvalidConfig(format!("sftp list: {e}")))?;
            let mut out = Vec::new();
            for entry in entries {
                let fname = entry.file_name();
                let meta = entry.metadata();
                let path = if remote_owned.ends_with('/') {
                    format!("{remote_owned}{fname}")
                } else {
                    format!("{remote_owned}/{fname}")
                };
                out.push(EntryMeta {
                    path,
                    is_dir: meta.is_dir(),
                    size: Some(meta.size.unwrap_or(0)),
                    last_modified: meta.mtime.map(|t| t.to_string()),
                    // SFTP has no server-side checksum headers.
                    etag: None,
                    content_md5: None,
                });
            }
            Ok((out, sftp))
        })
        .await
    }

    async fn stat(&self, path: &str) -> Result<Option<EntryMeta>, BackendError> {
        let remote = self.resolve(path);
        let remote_for_meta = remote.clone();
        self.with_session(|sftp| async move {
            let result = match sftp.metadata(&remote_for_meta).await {
                Ok(meta) => Some(EntryMeta {
                    path: remote_for_meta,
                    is_dir: meta.is_dir(),
                    size: Some(meta.size.unwrap_or(0)),
                    last_modified: meta.mtime.map(|t| t.to_string()),
                    etag: None,
                    content_md5: None,
                }),
                // Only a genuine "not there" is `Ok(None)`. A blanket
                // `Err(_) => None` reported permission-denied, a dropped
                // connection, and a protocol error as "the object does
                // not exist" — so an overwrite guard built on `stat`
                // would sail straight past a file it simply could not
                // read.
                Err(e) if is_absent(&e) => None,
                Err(e) => return Err(BackendError::Transport(e.to_string())),
            };
            Ok((result, sftp))
        })
        .await
    }

    async fn delete(&self, path: &str) -> Result<(), BackendError> {
        let remote = self.resolve(path);
        self.with_session(|sftp| async move {
            // Idempotent delete — a missing file is success, anything
            // else is not. The discarded result meant a delete refused
            // for permissions reported success and the caller moved on
            // believing the object was gone.
            match sftp.remove_file(&remote).await {
                Ok(()) => {}
                Err(e) if is_absent(&e) => {}
                Err(e) => return Err(BackendError::Transport(e.to_string())),
            }
            Ok(((), sftp))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `known_hosts` seeded by OpenSSH holds one line per key type
    /// for the same host. All of them must be honoured: the server
    /// decides which type it presents during negotiation, so returning
    /// only the first line refused hosts that were correctly pinned.
    #[test]
    fn known_hosts_returns_every_key_type_for_a_host() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("known_hosts");
        // Three distinct (valid-base64) key blobs for one host, plus a
        // line for an unrelated host that must not be picked up.
        std::fs::write(
            &path,
            concat!(
                "# comment line
",
                "example.com ssh-rsa AAAAB3NzaC1yc2EAAAA=
",
                "example.com ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTI=
",
                "example.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5
",
                "other.example ssh-ed25519 AAAAC3NzaC1lZDI1NTE6
",
            ),
        )
        .expect("write known_hosts");

        let fps =
            expected_host_key_fingerprints(&path, "example.com", 22).expect("read known_hosts");
        assert_eq!(fps.len(), 3, "one fingerprint per key type: {fps:?}");
        // Distinct key blobs must hash to distinct fingerprints, so a
        // server presenting any of the three verifies.
        let unique: std::collections::HashSet<&String> = fps.iter().collect();
        assert_eq!(unique.len(), 3, "fingerprints must be distinct");

        // An unpinned host yields an empty set, which fails closed.
        let none =
            expected_host_key_fingerprints(&path, "nobody.example", 22).expect("read known_hosts");
        assert!(none.is_empty());
    }

    /// Non-default ports are pinned under the bracketed form, and must
    /// not match the plain hostname line.
    #[test]
    fn known_hosts_matches_bracketed_port_form() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("known_hosts");
        std::fs::write(
            &path,
            concat!(
                "[example.com]:2222 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5
",
                "[example.com]:2222 ssh-rsa AAAAB3NzaC1yc2EAAAA=
",
            ),
        )
        .expect("write known_hosts");

        assert_eq!(
            expected_host_key_fingerprints(&path, "example.com", 2222)
                .expect("read")
                .len(),
            2,
        );
        assert!(
            expected_host_key_fingerprints(&path, "example.com", 22)
                .expect("read")
                .is_empty(),
            "port 22 must not match a :2222 pin",
        );
    }

    #[test]
    fn resolve_handles_empty_root_and_leading_slash() {
        let target = SftpTarget::new(
            "x",
            SftpConfig {
                host: "h".into(),
                port: 22,
                username: "u".into(),
                root: String::new(),
                known_hosts_path: String::new(),
            },
            "pw".into(),
        );
        assert_eq!(target.resolve("a/b.txt"), "/a/b.txt");
        assert_eq!(target.resolve("/a/b.txt"), "/a/b.txt");
    }

    #[test]
    fn resolve_honors_configured_root() {
        let target = SftpTarget::new(
            "x",
            SftpConfig {
                host: "h".into(),
                port: 22,
                username: "u".into(),
                root: "/home/freally".into(),
                known_hosts_path: String::new(),
            },
            "pw".into(),
        );
        assert_eq!(target.resolve("a/b.txt"), "/home/freally/a/b.txt");
        assert_eq!(target.resolve("/a/b.txt"), "/home/freally/a/b.txt");
    }

    #[test]
    fn hashed_known_hosts_field_matches_hmac_sha1_of_hostname() {
        use base64::Engine;
        use hmac::{Hmac, Mac};
        use sha1::Sha1;

        // Synthesize a hashed entry for `example.com`: salt is
        // 20 random bytes, hmac is HMAC-SHA1(salt, "example.com").
        let salt: [u8; 20] = *b"some-fixed-20-bytes!";
        let hostname = "example.com";
        let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(&salt).unwrap();
        mac.update(hostname.as_bytes());
        let hmac_bytes = mac.finalize().into_bytes();

        let salt_b64 = base64::engine::general_purpose::STANDARD.encode(salt);
        let hmac_b64 = base64::engine::general_purpose::STANDARD.encode(hmac_bytes);
        let hosts_field = format!("|1|{salt_b64}|{hmac_b64}");

        assert!(hashed_hostfield_matches(
            &hosts_field,
            hostname,
            "[example.com]:22"
        ));
        assert!(!hashed_hostfield_matches(
            &hosts_field,
            "other.com",
            "[other.com]:22"
        ));
    }

    #[test]
    fn debug_redacts_secret() {
        let target = SftpTarget::new(
            "sftp-test",
            SftpConfig {
                host: "h".into(),
                port: 22,
                username: "u".into(),
                root: String::new(),
                known_hosts_path: String::new(),
            },
            "super-secret-pw".into(),
        );
        let s = format!("{target:?}");
        assert!(
            !s.contains("super-secret-pw"),
            "Debug leaked the secret: {s}"
        );
    }
}
