//! Phase 51 — end-to-end encrypted collaboration.
//!
//! A [`Roster`] names the people who can read a shared item. Encrypting
//! through [`encrypt_for`] wraps a fresh per-file data key to every
//! member's X25519 public key via `age`, so each member decrypts with
//! their own secret and no shared passphrase ever exists.
//!
//! Removing a member mints a [`RevocationToken`] signed by the roster's
//! ed25519 admin key, so a removal cannot be forged by anything that
//! does not hold that key — see the limits below for exactly how far
//! that goes. New members are verified out of band with a
//! [`sas`] short authentication string — two people read the same short
//! code aloud and confirm they hold each other's real keys, which is
//! what defeats a substituted key.
//!
//! # What this does not do — read before trusting it
//!
//! **Revocation is forward-only.** age wraps a *per-file* key to the
//! recipients present at encrypt time. Removing a member bumps the
//! roster epoch so every *subsequent* encryption excludes them, but it
//! cannot claw back files they could already read: those bytes, and the
//! wrapped key inside them, were already in their hands. Nothing short
//! of re-encrypting and re-distributing every affected file changes
//! that, and even then the old ciphertext may have been copied. The API
//! deliberately has no `revoke_access_to_existing_files`, because that
//! function cannot be honestly implemented.
//!
//! **The roster is not itself confidential.** It carries labels and
//! public keys, which is metadata about who collaborates with whom.
//! Encrypt the roster file too if that matters.
//!
//! **The roster is trusted local state, not authenticated peer input.**
//! [`Roster::verify_revocations`] checks tokens against the roster's own
//! `admin_public` field, so it proves the file is internally consistent,
//! not where it came from — swap `admin_public` and re-sign, and it
//! still verifies. `members` is covered by no signature at all. That is
//! adequate today because the roster only ever lives in the per-user
//! config directory, and anyone who can write there can already do
//! worse. **Before any peer-import path is added**, the roster body
//! needs its own signature over `epoch + members`, and `admin_public`
//! must be pinned against the locally held key rather than believed.
//!
//! **This crate is a building block.** It is not yet wired into the
//! copy engine or the repository; those integrations are separate work.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::str::FromStr;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use freally_crypt::{EncryptionPolicy, Identity, Recipient, decrypted_reader, encrypted_writer};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Domain separator so a revocation signature can never be replayed as
/// a signature over some other structure.
const REVOKE_DOMAIN: &[u8] = b"freally-collab/revocation/v1";
/// Domain separator for the SAS derivation, for the same reason.
const SAS_DOMAIN: &[u8] = b"freally-collab/sas/v1";
/// Number of decimal groups in a short authentication string. Five
/// groups of three digits is ~40 bits — long enough that guessing a
/// collision in a live verification is hopeless, short enough to read
/// aloud over a phone call without anyone losing their place.
const SAS_GROUPS: usize = 5;

#[derive(Debug, Error)]
pub enum CollabError {
    #[error("`{0}` is not a valid age X25519 recipient")]
    InvalidRecipient(String),

    /// Deliberately carries no payload. The only value that can land
    /// here is the caller's `AGE-SECRET-KEY-1…` private key, and this
    /// message is handed straight to the UI (toasts, the webview
    /// console, log capture) — a secret that leaks that way cannot be
    /// rotated without re-issuing the member's identity.
    #[error("that is not a valid age X25519 identity")]
    InvalidIdentity,

    #[error("a member labelled `{0}` is already on the roster")]
    DuplicateMember(String),

    #[error("no member labelled `{0}`")]
    UnknownMember(String),

    #[error("refusing to remove the last member — the roster would be unreadable")]
    LastMember,

    #[error("revocation token for `{label}` failed signature verification")]
    BadRevocation { label: String },

    #[error("the roster carries no members to encrypt to")]
    EmptyRoster,

    #[error("crypto pipeline: {0}")]
    Crypt(String),

    #[error("roster serialisation: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("entropy source unavailable: {0}")]
    Entropy(String),
}

type Result<T> = std::result::Result<T, CollabError>;

// ---------------------------------------------------------------------
// Admin signing key
// ---------------------------------------------------------------------

/// The roster owner's ed25519 signing key.
///
/// `age` encrypts but does not sign, so membership changes are attested
/// separately. Only the admin can mint a revocation a peer will accept.
#[derive(Debug)]
pub struct AdminKey(SigningKey);

impl AdminKey {
    /// Generate a fresh admin key from OS entropy.
    pub fn generate() -> Result<Self> {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|e| CollabError::Entropy(e.to_string()))?;
        Ok(Self(SigningKey::from_bytes(&seed)))
    }

    /// Rebuild from a stored 32-byte seed.
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self(SigningKey::from_bytes(seed))
    }

    /// The seed, for the caller to store somewhere safe. This *is* the
    /// private key: anyone holding it can forge revocations.
    pub fn seed(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    /// The public half a peer verifies against.
    pub fn public(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }
}

// ---------------------------------------------------------------------
// Members + roster
// ---------------------------------------------------------------------

/// One collaborator: a human-readable label and their age X25519 public
/// key. Never carries a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub label: String,
    /// `age1…` recipient string.
    pub recipient: String,
}

/// A signed statement that a member was removed at a given epoch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevocationToken {
    pub label: String,
    /// Roster epoch this revocation takes effect at.
    pub epoch: u64,
    /// Hex ed25519 signature over the canonical encoding below.
    pub signature: String,
}

impl RevocationToken {
    /// Canonical bytes the signature covers. Length-prefixing the label
    /// stops `("ab", 1)` and `("a", …)` colliding into the same message.
    fn signing_input(label: &str, epoch: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(REVOKE_DOMAIN.len() + label.len() + 16);
        out.extend_from_slice(REVOKE_DOMAIN);
        out.extend_from_slice(&(label.len() as u64).to_le_bytes());
        out.extend_from_slice(label.as_bytes());
        out.extend_from_slice(&epoch.to_le_bytes());
        out
    }

    /// Verify against the roster admin's public key.
    pub fn verify(&self, admin_public: &[u8; 32]) -> bool {
        let Ok(vk) = VerifyingKey::from_bytes(admin_public) else {
            return false;
        };
        let Some(raw) = hex_to_bytes(&self.signature) else {
            return false;
        };
        let Ok(bytes): std::result::Result<[u8; 64], _> = raw.try_into() else {
            return false;
        };
        let sig = Signature::from_bytes(&bytes);
        vk.verify(&Self::signing_input(&self.label, self.epoch), &sig)
            .is_ok()
    }
}

/// The set of people who can read items encrypted through this roster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Roster {
    /// Bumped on every membership change. Encrypting stamps the epoch
    /// so a reader can tell which generation a file belongs to.
    pub epoch: u64,
    pub members: Vec<Member>,
    /// Hex ed25519 public key of the admin who signs revocations.
    pub admin_public: String,
    /// Every revocation ever issued, kept so a peer syncing an older
    /// roster can see who was removed and when.
    pub revocations: Vec<RevocationToken>,
}

impl Roster {
    /// A new, empty roster owned by `admin`.
    pub fn new(admin: &AdminKey) -> Self {
        Self {
            epoch: 1,
            members: Vec::new(),
            admin_public: bytes_to_hex(&admin.public()),
            revocations: Vec::new(),
        }
    }

    /// Add a member. Rejects a duplicate label and a recipient string
    /// `age` cannot parse — a typo'd key would silently make every
    /// future file unreadable by that person.
    pub fn add_member(&mut self, label: &str, recipient: &str) -> Result<()> {
        if self.members.iter().any(|m| m.label == label) {
            return Err(CollabError::DuplicateMember(label.to_string()));
        }
        age::x25519::Recipient::from_str(recipient.trim())
            .map_err(|_| CollabError::InvalidRecipient(recipient.to_string()))?;
        self.members.push(Member {
            label: label.to_string(),
            recipient: recipient.trim().to_string(),
        });
        self.epoch += 1;
        Ok(())
    }

    /// Remove a member and mint a signed revocation.
    ///
    /// Forward-only: see the crate docs. This changes who can read
    /// files encrypted *after* it, and nothing about files already out.
    pub fn remove_member(&mut self, label: &str, admin: &AdminKey) -> Result<RevocationToken> {
        let idx = self
            .members
            .iter()
            .position(|m| m.label == label)
            .ok_or_else(|| CollabError::UnknownMember(label.to_string()))?;
        if self.members.len() == 1 {
            return Err(CollabError::LastMember);
        }
        self.members.remove(idx);
        self.epoch += 1;
        let sig = admin
            .0
            .sign(&RevocationToken::signing_input(label, self.epoch));
        let token = RevocationToken {
            label: label.to_string(),
            epoch: self.epoch,
            signature: bytes_to_hex(&sig.to_bytes()),
        };
        self.revocations.push(token.clone());
        Ok(token)
    }

    /// Bump the epoch without changing membership.
    ///
    /// Use after re-keying a member: subsequent files are stamped with
    /// the new generation even though the member set is unchanged.
    pub fn rotate(&mut self) -> u64 {
        self.epoch += 1;
        self.epoch
    }

    /// The crypt-layer recipients for the current member set.
    pub fn recipients(&self) -> Vec<Recipient> {
        self.members
            .iter()
            .map(|m| Recipient::X25519(m.recipient.clone()))
            .collect()
    }

    /// Check every stored revocation against the admin key.
    ///
    /// A roster arriving from a peer is untrusted input; one forged
    /// revocation would let an attacker quietly drop a member.
    pub fn verify_revocations(&self) -> Result<()> {
        let Some(admin) = hex_to_bytes(&self.admin_public) else {
            return Err(CollabError::BadRevocation {
                label: "<malformed admin key>".to_string(),
            });
        };
        let Ok(admin): std::result::Result<[u8; 32], _> = admin.try_into() else {
            return Err(CollabError::BadRevocation {
                label: "<malformed admin key>".to_string(),
            });
        };
        for token in &self.revocations {
            if !token.verify(&admin) {
                return Err(CollabError::BadRevocation {
                    label: token.label.clone(),
                });
            }
        }
        Ok(())
    }

    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn from_json(s: &str) -> Result<Self> {
        Ok(serde_json::from_str(s)?)
    }
}

// ---------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------

/// Encrypt `plaintext` to every member of `roster`.
///
/// `age` generates a fresh data key per call and wraps it once per
/// recipient, so adding a member costs one stanza, not a re-encryption
/// of the payload.
///
/// Compression before encryption is switched **off**: a shared document
/// can mix attacker-influenced text with secret text in one stream,
/// which is the CRIME shape `EncryptionPolicy` warns about.
pub fn encrypt_for(roster: &Roster, plaintext: &[u8]) -> Result<Vec<u8>> {
    if roster.members.is_empty() {
        return Err(CollabError::EmptyRoster);
    }
    let mut policy = EncryptionPolicy::strict(roster.recipients());
    policy.allow_compression_before_encrypt = false;

    let mut sink =
        encrypted_writer(Vec::new(), &policy).map_err(|e| CollabError::Crypt(e.to_string()))?;
    sink.write_all(plaintext)
        .map_err(|e| CollabError::Crypt(e.to_string()))?;
    sink.finish().map_err(|e| CollabError::Crypt(e.to_string()))
}

/// Decrypt with one member's age X25519 secret (`AGE-SECRET-KEY-1…`).
pub fn decrypt_with(identity_secret: &str, ciphertext: &[u8]) -> Result<Vec<u8>> {
    let identity = Identity::new()
        .with_x25519(identity_secret.trim())
        .map_err(|_| CollabError::InvalidIdentity)?;
    // `decrypted_reader` boxes the reader, so it needs owned `'static`
    // data — a borrowed slice cannot satisfy that bound.
    let mut reader = decrypted_reader(std::io::Cursor::new(ciphertext.to_vec()), &identity)
        .map_err(|e| CollabError::Crypt(e.to_string()))?;
    let mut out = Vec::new();
    reader
        .read_to_end(&mut out)
        .map_err(|e| CollabError::Crypt(e.to_string()))?;
    Ok(out)
}

/// Generate a fresh member keypair: `(secret, recipient)`.
pub fn generate_member() -> (String, String) {
    let id = age::x25519::Identity::generate();
    let pubkey = id.to_public().to_string();
    (id.to_string().expose_secret().to_string(), pubkey)
}

// ---------------------------------------------------------------------
// SAS — short authentication string
// ---------------------------------------------------------------------

/// A short code two people compare out of band to confirm they hold
/// each other's real keys.
///
/// Order-independent: both sides derive the same code regardless of who
/// is "a" and who is "b", so neither has to agree an ordering first.
/// Comparing this over an *already authenticated* channel (a phone call
/// where you recognise the voice) is what defeats a substituted key —
/// an attacker in the middle holds different keys and cannot make the
/// codes match.
pub fn sas(recipient_a: &str, recipient_b: &str) -> String {
    // Sorting is what makes it order-independent. A BTreeSet also
    // collapses the degenerate a == b case to a single input, which is
    // fine: verifying a key against itself is meaningless anyway.
    let ordered: BTreeSet<&str> = [recipient_a.trim(), recipient_b.trim()]
        .into_iter()
        .collect();
    let mut hasher = blake3::Hasher::new();
    hasher.update(SAS_DOMAIN);
    for key in ordered {
        hasher.update(&(key.len() as u64).to_le_bytes());
        hasher.update(key.as_bytes());
    }
    let digest = hasher.finalize();
    let bytes = digest.as_bytes();
    let mut groups = Vec::with_capacity(SAS_GROUPS);
    for b in bytes.iter().take(SAS_GROUPS) {
        groups.push(format!("{b:03}"));
    }
    groups.join("-")
}

// ---------------------------------------------------------------------
// hex helpers (no new dependency for two tiny functions)
// ---------------------------------------------------------------------

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn hex_to_bytes(s: &str) -> Option<Vec<u8>> {
    // Index the bytes, not the `str`. `&s[i..i + 2]` splits on byte
    // offsets, so a multi-byte char straddling the boundary panics —
    // and this parses values that come off disk, where a hand-edited
    // or truncated file can put one there.
    let raw = s.as_bytes();
    if raw.len() % 2 != 0 {
        return None;
    }
    raw.chunks(2)
        .map(|pair| {
            // `from_str_radix` also accepts a leading `+`, which is not
            // hex; reject anything that is not two hex digits.
            if !pair.iter().all(u8::is_ascii_hexdigit) {
                return None;
            }
            u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()
        })
        .collect()
}

use age::secrecy::ExposeSecret;

#[cfg(test)]
mod tests {
    use super::*;

    fn roster_with(n: usize) -> (Roster, AdminKey, Vec<(String, String)>) {
        let admin = AdminKey::generate().unwrap();
        let mut roster = Roster::new(&admin);
        let mut keys = Vec::new();
        for i in 0..n {
            let (secret, recipient) = generate_member();
            roster
                .add_member(&format!("member{i}"), &recipient)
                .unwrap();
            keys.push((secret, recipient));
        }
        (roster, admin, keys)
    }

    #[test]
    fn every_member_can_decrypt_and_an_outsider_cannot() {
        let (roster, _admin, keys) = roster_with(3);
        let ciphertext = encrypt_for(&roster, b"shared secret").unwrap();

        for (secret, _) in &keys {
            let plain = decrypt_with(secret, &ciphertext).unwrap();
            assert_eq!(plain, b"shared secret");
        }

        // Someone never added to the roster holds a valid age key and
        // still cannot read it.
        let (outsider_secret, _) = generate_member();
        assert!(decrypt_with(&outsider_secret, &ciphertext).is_err());
    }

    #[test]
    fn removal_is_forward_only_and_the_docs_say_so() {
        let (mut roster, admin, keys) = roster_with(2);
        let before = encrypt_for(&roster, b"old file").unwrap();

        roster.remove_member("member0", &admin).unwrap();
        let after = encrypt_for(&roster, b"new file").unwrap();

        // Removed member is locked out of anything encrypted after…
        assert!(decrypt_with(&keys[0].0, &after).is_err());
        // …but the file they already had access to is still readable by
        // them. This is the honest limit documented at the crate root.
        assert_eq!(decrypt_with(&keys[0].0, &before).unwrap(), b"old file");
        // The remaining member reads both.
        assert_eq!(decrypt_with(&keys[1].0, &after).unwrap(), b"new file");
    }

    /// Regression — `hex_to_bytes` used to slice the `str` by byte
    /// offset, so a multi-byte char straddling the split panicked
    /// instead of returning `None`. The input comes off disk, where a
    /// truncated or hand-edited roster can easily put one there.
    #[test]
    fn hex_to_bytes_rejects_junk_instead_of_panicking() {
        // Multi-byte chars: `€` is 3 bytes, so these are even-length in
        // bytes and reach the slicing path.
        assert_eq!(hex_to_bytes("€x"), None);
        assert_eq!(hex_to_bytes("€€"), None);
        assert_eq!(hex_to_bytes("ab€xcd"), None);

        // `from_str_radix` accepts a leading `+`; hex does not.
        assert_eq!(hex_to_bytes("+5"), None);

        // Odd length and non-hex still rejected.
        assert_eq!(hex_to_bytes("abc"), None);
        assert_eq!(hex_to_bytes("zz"), None);

        // And valid hex still round-trips, upper or lower case.
        assert_eq!(hex_to_bytes("00ff"), Some(vec![0x00, 0xff]));
        assert_eq!(hex_to_bytes("00FF"), Some(vec![0x00, 0xff]));
        assert_eq!(hex_to_bytes(""), Some(vec![]));
        let seed = [7u8; 32];
        assert_eq!(hex_to_bytes(&bytes_to_hex(&seed)), Some(seed.to_vec()));
    }

    #[test]
    fn revocation_is_signed_and_tampering_is_detected() {
        let (mut roster, admin, _keys) = roster_with(2);
        let token = roster.remove_member("member0", &admin).unwrap();
        let admin_pub: [u8; 32] = hex_to_bytes(&roster.admin_public)
            .unwrap()
            .try_into()
            .unwrap();
        assert!(token.verify(&admin_pub));
        roster.verify_revocations().unwrap();

        // Re-pointing a genuine signature at a different member must fail.
        let mut forged = token.clone();
        forged.label = "member1".to_string();
        assert!(!forged.verify(&admin_pub));

        // So must replaying it at a different epoch.
        let mut replayed = token;
        replayed.epoch += 1;
        assert!(!replayed.verify(&admin_pub));

        // And a roster carrying a forged token is rejected wholesale.
        roster.revocations.push(forged);
        assert!(roster.verify_revocations().is_err());
    }

    #[test]
    fn a_different_admin_cannot_sign_a_revocation() {
        let (mut roster, admin, _keys) = roster_with(2);
        let attacker = AdminKey::generate().unwrap();
        let token = roster.remove_member("member0", &admin).unwrap();
        assert!(!token.verify(&attacker.public()));
    }

    #[test]
    fn sas_is_order_independent_and_key_specific() {
        let (_, a) = generate_member();
        let (_, b) = generate_member();
        let (_, c) = generate_member();

        assert_eq!(sas(&a, &b), sas(&b, &a), "SAS must not depend on order");
        assert_ne!(sas(&a, &b), sas(&a, &c), "different keys must differ");

        // Shape: five 3-digit groups, readable aloud.
        let code = sas(&a, &b);
        let parts: Vec<&str> = code.split('-').collect();
        assert_eq!(parts.len(), SAS_GROUPS);
        assert!(
            parts
                .iter()
                .all(|p| p.len() == 3 && p.chars().all(|c| c.is_ascii_digit()))
        );
    }

    #[test]
    fn roster_rejects_a_typod_recipient_and_duplicate_labels() {
        let admin = AdminKey::generate().unwrap();
        let mut roster = Roster::new(&admin);
        let (_, good) = generate_member();

        assert!(matches!(
            roster.add_member("a", "age1-not-a-real-key"),
            Err(CollabError::InvalidRecipient(_))
        ));
        roster.add_member("a", &good).unwrap();
        assert!(matches!(
            roster.add_member("a", &good),
            Err(CollabError::DuplicateMember(_))
        ));
    }

    #[test]
    fn last_member_cannot_be_removed() {
        let (mut roster, admin, _keys) = roster_with(1);
        assert!(matches!(
            roster.remove_member("member0", &admin),
            Err(CollabError::LastMember)
        ));
    }

    #[test]
    fn empty_roster_refuses_to_encrypt() {
        let admin = AdminKey::generate().unwrap();
        let roster = Roster::new(&admin);
        assert!(matches!(
            encrypt_for(&roster, b"x"),
            Err(CollabError::EmptyRoster)
        ));
    }

    #[test]
    fn roster_round_trips_through_json() {
        let (mut roster, admin, _keys) = roster_with(2);
        roster.remove_member("member0", &admin).unwrap();
        let json = roster.to_json().unwrap();
        let back = Roster::from_json(&json).unwrap();
        assert_eq!(roster, back);
        back.verify_revocations().unwrap();
    }

    #[test]
    fn epoch_advances_on_every_membership_change() {
        let admin = AdminKey::generate().unwrap();
        let mut roster = Roster::new(&admin);
        let start = roster.epoch;
        let (_, r1) = generate_member();
        let (_, r2) = generate_member();
        roster.add_member("a", &r1).unwrap();
        roster.add_member("b", &r2).unwrap();
        assert_eq!(roster.epoch, start + 2);
        roster.remove_member("a", &admin).unwrap();
        assert_eq!(roster.epoch, start + 3);
        assert_eq!(roster.rotate(), start + 4);
    }
}
