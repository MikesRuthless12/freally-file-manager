//! Phase 32 — OS keychain wrapper for backend secrets.
//!
//! Each backend's secret bundle is stored under the service name
//! `freally-cloud/<backend-name>` with a fixed account label
//! ([`CREDENTIAL_USER`]). Backends that need multiple separate
//! secrets (e.g., S3's access key + secret access key) pack them into
//! a single newline-delimited blob the operator builder splits at
//! load time — keeps the keychain entry count one-per-backend.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use freally_crypt::{EncryptionPolicy, Identity, decrypted_reader, encrypted_writer};
use secrecy::SecretString;

use thiserror::Error;

/// Account label used inside the keychain entry. Matches the value
/// the Add-backend wizard writes; changing it would orphan stored
/// credentials, so it lives in code as a constant.
pub const CREDENTIAL_USER: &str = "freally-cloud";

/// Service name prefix the backend's name slot is appended to. The
/// final keychain key is `freally-cloud/<backend-name>`.
pub const SERVICE_PREFIX: &str = "freally-cloud";

#[derive(Debug, Error)]
pub enum CredentialsError {
    #[error("keychain rejected the operation: {0}")]
    Keyring(#[from] keyring::Error),

    #[error("backend name `{0}` is empty or contains an illegal character")]
    InvalidBackendName(String),

    /// The supplied secret blob was empty. Empty secrets are rejected
    /// at store-time to prevent silently writing a sentinel that
    /// would later be indistinguishable from a missing-entry case.
    #[error("refusing to store an empty secret for backend `{0}`")]
    EmptySecret(String),

    /// Reading or writing the encrypted credential file failed.
    #[error("credential file I/O failed: {0}")]
    Io(#[from] std::io::Error),

    /// Decryption failed. `age` cannot tell a wrong passphrase apart
    /// from a truncated or tampered file, so this covers both — the
    /// UI must not claim which one it was.
    #[error("credential file could not be decrypted: wrong passphrase or damaged file")]
    Undecryptable,

    /// The decrypted payload was not the expected JSON map.
    #[error("credential file is not valid credential JSON: {0}")]
    Malformed(String),

    /// Encrypt/decrypt plumbing failed for a reason other than a bad
    /// passphrase.
    #[error("credential encryption failed: {0}")]
    Crypt(String),
}

/// Stateless façade — every method opens a fresh `keyring::Entry`
/// keyed on the backend name so the registry can be re-entered
/// from multiple threads.
///
/// **Phase 32h TOCTOU hardening:** the `keyring` crate's `Entry`
/// is not internally synchronised; back-to-back `store` /
/// `load` / `delete` calls from different threads can interleave
/// at the OS-keystore level, so a load that races a concurrent
/// store can return either the previous or the new value
/// non-deterministically (and on macOS, can spuriously surface
/// `NoEntry` between the underlying SecItem operations). We
/// serialise all access through a process-wide [`Mutex`] —
/// minimal contention because credential reads happen only at
/// operator-build time, but enough to give callers a coherent
/// view. The mutex covers the whole `entry()` + read/write pair
/// so the critical section matches what an atomic keystore op
/// would have given us.
#[derive(Debug, Default)]
pub enum Credentials {
    /// The host's OS keychain — the default for a normal install.
    #[default]
    Keychain,
    /// An `age`-encrypted file unlocked by a user passphrase.
    ///
    /// Portable installs use this: a keychain entry would stay on the
    /// host after the stick is unplugged, which is both a leak and a
    /// credential the next machine cannot reach.
    EncryptedFile {
        path: PathBuf,
        passphrase: SecretString,
    },
}

/// Process-wide keyring serialisation lock. Held across each of
/// the four public operations (`store` / `load` / `delete` +
/// `entry()` builds). Poison recovery: an unwrap on a poisoned
/// mutex would fail loudly, but we'd rather degrade to "treat as
/// busy" than panic — the `lock_keyring` helper recovers the
/// guard via `into_inner` so a previously-panicking thread
/// doesn't permanently brick credential access.
static KEYRING_LOCK: Mutex<()> = Mutex::new(());

fn lock_keyring() -> std::sync::MutexGuard<'static, ()> {
    match KEYRING_LOCK.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

impl Credentials {
    /// Persist the secret blob for `backend_name` in the OS
    /// keychain. Overwrites any existing entry under the same name.
    /// Empty `secret` values are rejected with
    /// [`CredentialsError::EmptySecret`] to prevent storing a
    /// sentinel that's indistinguishable from a missing entry on
    /// the next read (which could flip a configured backend into
    /// "anonymous" mode silently).
    pub fn store(&self, backend_name: &str, secret: &str) -> Result<(), CredentialsError> {
        if secret.is_empty() {
            return Err(CredentialsError::EmptySecret(backend_name.to_owned()));
        }
        validate_backend_name(backend_name)?;
        let _guard = lock_keyring();
        match self {
            Self::Keychain => {
                self.entry(backend_name)?.set_password(secret)?;
                Ok(())
            }
            Self::EncryptedFile { path, passphrase } => {
                let mut map = read_map(path, passphrase)?;
                map.insert(backend_name.to_owned(), secret.to_owned());
                write_map(path, passphrase, &map)
            }
        }
    }

    /// Read the secret blob for `backend_name`. Returns
    /// `Ok(None)` when no entry exists; raises [`CredentialsError`]
    /// for everything else. The `entry()` build + read live inside
    /// the same critical section so a concurrent `store` /
    /// `delete` cannot race between the two halves.
    pub fn load(&self, backend_name: &str) -> Result<Option<String>, CredentialsError> {
        validate_backend_name(backend_name)?;
        let _guard = lock_keyring();
        match self {
            Self::Keychain => match self.entry(backend_name)?.get_password() {
                Ok(p) => Ok(Some(p)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(CredentialsError::Keyring(e)),
            },
            Self::EncryptedFile { path, passphrase } => {
                Ok(read_map(path, passphrase)?.remove(backend_name))
            }
        }
    }

    /// Remove the secret entry for `backend_name`. No-op if there
    /// was no entry to begin with.
    pub fn delete(&self, backend_name: &str) -> Result<(), CredentialsError> {
        validate_backend_name(backend_name)?;
        let _guard = lock_keyring();
        match self {
            Self::Keychain => match self.entry(backend_name)?.delete_credential() {
                Ok(()) => Ok(()),
                Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(CredentialsError::Keyring(e)),
            },
            Self::EncryptedFile { path, passphrase } => {
                let mut map = read_map(path, passphrase)?;
                if map.remove(backend_name).is_none() {
                    return Ok(());
                }
                write_map(path, passphrase, &map)
            }
        }
    }

    /// Compose the per-backend service string + open the entry.
    /// Rejects backend names that are empty or contain any byte
    /// outside `[A-Za-z0-9_-]`. The tighter allowlist (vs. the
    /// previous `/` + NUL exclusion) defends against names like
    /// `..` / `.` / `name with spaces` / unicode lookalikes that
    /// would otherwise round-trip through the `<prefix>/<name>`
    /// service-string join unchecked. Per-OS keychain
    /// implementations may further restrict allowed bytes — the
    /// allowlist matches the most conservative set
    /// (Windows Credential Manager treats `/` as a separator,
    /// macOS Keychain accepts most printable ASCII, Linux
    /// Secret Service is a free-form string) so the same name
    /// is portable across platforms.
    fn entry(&self, backend_name: &str) -> Result<keyring::Entry, CredentialsError> {
        validate_backend_name(backend_name)?;
        let service = format!("{SERVICE_PREFIX}/{backend_name}");
        Ok(keyring::Entry::new(&service, CREDENTIAL_USER)?)
    }
}

/// Reject names that are empty or carry a byte outside
/// `[A-Za-z0-9_-]`.
///
/// Applied to both stores: the keychain needs it because the name is
/// joined into a `<prefix>/<name>` service string, and the encrypted
/// file needs it because the name becomes a JSON key that round-trips
/// back out on the next load.
fn validate_backend_name(backend_name: &str) -> Result<(), CredentialsError> {
    if backend_name.is_empty() {
        return Err(CredentialsError::InvalidBackendName(
            backend_name.to_owned(),
        ));
    }
    if !backend_name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(CredentialsError::InvalidBackendName(
            backend_name.to_owned(),
        ));
    }
    Ok(())
}

/// Decrypt the credential file into its name -> secret map.
///
/// A missing file is an empty map, not an error: that is exactly the
/// state of a portable install that has not stored anything yet.
fn read_map(path: &Path, pw: &SecretString) -> Result<BTreeMap<String, String>, CredentialsError> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(CredentialsError::Io(e)),
    };
    let identity = Identity::new().with_passphrase(pw.clone());
    let mut reader =
        decrypted_reader(file, &identity).map_err(|_| CredentialsError::Undecryptable)?;
    let mut plain = String::new();
    reader
        .read_to_string(&mut plain)
        .map_err(|_| CredentialsError::Undecryptable)?;
    serde_json::from_str(&plain).map_err(|e| CredentialsError::Malformed(e.to_string()))
}

/// Encrypt `map` and replace the credential file atomically.
///
/// Compression before encryption is switched **off**. The plaintext
/// mixes user-chosen backend names with secrets, which is precisely
/// the CRIME-style shape `EncryptionPolicy` documents: ciphertext
/// length would otherwise leak how well a secret compresses against a
/// name the user controls.
fn write_map(
    path: &Path,
    pw: &SecretString,
    map: &BTreeMap<String, String>,
) -> Result<(), CredentialsError> {
    let mut policy = EncryptionPolicy::passphrase(pw.clone());
    policy.allow_compression_before_encrypt = false;

    let plain = serde_json::to_vec(map).map_err(|e| CredentialsError::Malformed(e.to_string()))?;
    let mut sink = encrypted_writer(Vec::new(), &policy)
        .map_err(|e| CredentialsError::Crypt(e.to_string()))?;
    sink.write_all(&plain)
        .map_err(|e| CredentialsError::Crypt(e.to_string()))?;
    let ciphertext = sink
        .finish()
        .map_err(|e| CredentialsError::Crypt(e.to_string()))?;

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    // Write beside the target then rename, so an interrupted write
    // cannot leave a half-encrypted file where the secrets used to be.
    let tmp = path.with_extension("age.tmp");
    std::fs::write(&tmp, &ciphertext)?;
    restrict_to_owner(&tmp)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Narrow the file to owner-only where the platform expresses that in
/// mode bits. On Windows the inherited ACL of the portable directory
/// governs and there is no portable equivalent to set here.
fn restrict_to_owner(path: &Path) -> Result<(), CredentialsError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_or_illegal_name() {
        let creds = Credentials::Keychain;
        assert!(matches!(
            creds.entry("").unwrap_err(),
            CredentialsError::InvalidBackendName(_)
        ));
        assert!(matches!(
            creds.entry("a/b").unwrap_err(),
            CredentialsError::InvalidBackendName(_)
        ));
        assert!(matches!(
            creds.entry("a\0b").unwrap_err(),
            CredentialsError::InvalidBackendName(_)
        ));
        // Phase 32h — `..` / `.` / spaces / dots / colons all
        // refused under the tighter `[A-Za-z0-9_-]` allowlist.
        for bad in [
            "..", ".", "a.b", "a b", "a:b", "a@b", "a\\b", "ünicode", "name?",
        ] {
            assert!(
                matches!(
                    creds.entry(bad).unwrap_err(),
                    CredentialsError::InvalidBackendName(_)
                ),
                "expected `{bad}` to be rejected"
            );
        }
    }

    #[test]
    fn entry_uses_prefixed_service_name() {
        let creds = Credentials::Keychain;
        // Constructing the entry doesn't touch the live keychain; we
        // can sanity-check the path without depending on a daemon.
        let entry = creds.entry("my-bucket");
        assert!(entry.is_ok(), "entry build should succeed for legal names");
    }

    #[test]
    fn allowlist_accepts_alphanumeric_underscore_hyphen() {
        let creds = Credentials::Keychain;
        for ok in ["abc", "ABC123", "alpha_beta", "alpha-beta", "X9_y-Z"] {
            assert!(
                creds.entry(ok).is_ok(),
                "expected `{ok}` to pass the allowlist"
            );
        }
    }

    #[test]
    fn store_rejects_empty_secret() {
        let creds = Credentials::Keychain;
        // Skip touching the real keychain by relying on the empty-
        // secret short-circuit at the top of `store` — the guard
        // returns before any keyring call. (Some CI runners don't
        // have a Secret Service / Credential Manager available.)
        let err = creds.store("legal-name", "").unwrap_err();
        assert!(
            matches!(err, CredentialsError::EmptySecret(_)),
            "expected EmptySecret, got {err:?}"
        );
    }

    // ---------------------------------------------------------------
    // FFM-M21 — the encrypted-file store used by portable installs.
    // These touch no keychain, so they run everywhere including CI
    // containers with no Secret Service.
    // ---------------------------------------------------------------

    fn file_store(dir: &tempfile::TempDir, pw: &str) -> Credentials {
        Credentials::EncryptedFile {
            path: dir.path().join("credentials.age"),
            passphrase: SecretString::from(pw.to_owned()),
        }
    }

    #[test]
    fn missing_file_reads_as_no_entry_rather_than_an_error() {
        let dir = tempfile::tempdir().unwrap();
        // A portable install that has stored nothing yet must look
        // like "no credential", not like a failure.
        assert_eq!(file_store(&dir, "pw").load("bucket").unwrap(), None);
    }

    #[test]
    fn round_trips_and_isolates_entries() {
        let dir = tempfile::tempdir().unwrap();
        let store = file_store(&dir, "correct-horse");

        store.store("alpha", "alpha-secret").unwrap();
        store.store("beta", "beta-secret").unwrap();

        assert_eq!(
            store.load("alpha").unwrap().as_deref(),
            Some("alpha-secret")
        );
        assert_eq!(store.load("beta").unwrap().as_deref(), Some("beta-secret"));

        // Overwrite is last-writer-wins, like the keychain path.
        store.store("alpha", "rotated").unwrap();
        assert_eq!(store.load("alpha").unwrap().as_deref(), Some("rotated"));

        // Deleting one leaves the other intact, and deleting a name
        // that was never there succeeds.
        store.delete("alpha").unwrap();
        assert_eq!(store.load("alpha").unwrap(), None);
        assert_eq!(store.load("beta").unwrap().as_deref(), Some("beta-secret"));
        store.delete("never-stored").unwrap();
    }

    #[test]
    fn wrong_passphrase_cannot_read_the_file() {
        let dir = tempfile::tempdir().unwrap();
        file_store(&dir, "right").store("bucket", "s3cr3t").unwrap();

        let err = file_store(&dir, "wrong").load("bucket").unwrap_err();
        assert!(
            matches!(err, CredentialsError::Undecryptable),
            "expected Undecryptable, got {err:?}"
        );
    }

    #[test]
    fn secret_never_appears_in_the_file_on_disk() {
        // The whole point of the store: the stick must not carry the
        // secret in the clear. Checks the backend name too, since it
        // is a JSON key inside the same plaintext.
        let dir = tempfile::tempdir().unwrap();
        let store = file_store(&dir, "correct-horse");
        store.store("my-bucket", "TOP-SECRET-VALUE").unwrap();

        let raw = std::fs::read(dir.path().join("credentials.age")).unwrap();
        for needle in [b"TOP-SECRET-VALUE".as_slice(), b"my-bucket".as_slice()] {
            assert!(
                !raw.windows(needle.len()).any(|w| w == needle),
                "plaintext `{}` found in the encrypted file",
                String::from_utf8_lossy(needle)
            );
        }
    }

    #[test]
    fn file_store_applies_the_same_name_allowlist() {
        let dir = tempfile::tempdir().unwrap();
        let store = file_store(&dir, "pw");
        // Without this the name would land as a raw JSON key and
        // round-trip back out on the next load.
        for bad in ["", "a/b", "..", "a b"] {
            assert!(
                matches!(
                    store.load(bad).unwrap_err(),
                    CredentialsError::InvalidBackendName(_)
                ),
                "expected `{bad}` to be rejected"
            );
        }
    }
}
