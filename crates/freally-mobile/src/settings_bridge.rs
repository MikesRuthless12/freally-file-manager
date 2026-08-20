//! Bridge between `freally-settings::MobileSettings` (the
//! persistence shape, stringly-typed) and the runtime
//! [`crate::MobileSettings`] / [`crate::PairingRecord`] /
//! [`crate::PushTarget`] shape.

use freally_settings::{MobilePairingEntry, MobilePushTarget};

use crate::notify::PushTarget;
use crate::pairing::PairingRecord;
use crate::settings::MobileSettings;

/// Convert a persisted [`freally_settings::MobileSettings`] into the
/// runtime [`MobileSettings`].
pub fn from_persisted(persisted: &freally_settings::MobileSettings) -> MobileSettings {
    MobileSettings {
        pair_enabled: persisted.pair_enabled,
        auto_connect: persisted.auto_connect,
        peerjs_broker: persisted.peerjs_broker.clone(),
        desktop_peer_id: persisted.desktop_peer_id.clone(),
        pairings: persisted
            .pairings
            .iter()
            .map(pairing_from_persisted)
            .collect(),
    }
}

/// Inverse of [`from_persisted`]. Provider credential strings are
/// preserved verbatim because the runtime shape doesn't carry them
/// (they live only in the persisted shape today; the Phase 37
/// follow-up will move them to the OS keychain).
pub fn to_persisted(
    runtime: &MobileSettings,
    apns_p8_pem: String,
    apns_team_id: String,
    apns_key_id: String,
    fcm_service_account_json: String,
) -> freally_settings::MobileSettings {
    freally_settings::MobileSettings {
        pair_enabled: runtime.pair_enabled,
        auto_connect: runtime.auto_connect,
        peerjs_broker: runtime.peerjs_broker.clone(),
        desktop_peer_id: runtime.desktop_peer_id.clone(),
        pairings: runtime.pairings.iter().map(pairing_to_persisted).collect(),
        apns_p8_pem,
        apns_team_id,
        apns_key_id,
        fcm_service_account_json,
    }
}

fn pairing_from_persisted(entry: &MobilePairingEntry) -> PairingRecord {
    PairingRecord {
        label: entry.label.clone(),
        phone_public_key: hex_decode_32(&entry.phone_public_key_hex).unwrap_or([0u8; 32]),
        paired_at: entry.paired_at,
        push_target: entry.push_target.as_ref().map(push_target_from_persisted),
    }
}

fn pairing_to_persisted(record: &PairingRecord) -> MobilePairingEntry {
    MobilePairingEntry {
        label: record.label.clone(),
        phone_public_key_hex: hex_encode_32(&record.phone_public_key),
        paired_at: record.paired_at,
        push_target: record.push_target.as_ref().map(push_target_to_persisted),
    }
}

fn push_target_from_persisted(t: &MobilePushTarget) -> PushTarget {
    match t {
        MobilePushTarget::Apns { token } => PushTarget::Apns {
            token: token.clone(),
        },
        MobilePushTarget::Fcm { token } => PushTarget::Fcm {
            token: token.clone(),
        },
        MobilePushTarget::StubEndpoint { url } => PushTarget::StubEndpoint { url: url.clone() },
    }
}

fn push_target_to_persisted(t: &PushTarget) -> MobilePushTarget {
    match t {
        PushTarget::Apns { token } => MobilePushTarget::Apns {
            token: token.clone(),
        },
        PushTarget::Fcm { token } => MobilePushTarget::Fcm {
            token: token.clone(),
        },
        PushTarget::StubEndpoint { url } => MobilePushTarget::StubEndpoint { url: url.clone() },
    }
}

// Both codecs live in `server`, which already had the byte-indexed
// forms; this module had grown its own pair.
fn hex_encode_32(b: &[u8; 32]) -> String {
    crate::server::hex_lower(b)
}

/// Decode a 64-character hex string into 32 bytes, or `None` if it is
/// not exactly that.
///
/// This had its own byte-index loop guarded by `str::len` — a *byte*
/// count — so a 64-byte string of multi-byte characters cleared the
/// guard and then panicked slicing across a character boundary. A
/// pairing key arrives from the on-disk settings file and over the
/// pairing wire, so that input is reachable, not hypothetical.
/// `server::decode_hex_array` was already byte-indexed and correct.
fn hex_decode_32(s: &str) -> Option<[u8; 32]> {
    crate::server::decode_hex_array::<32>(s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The length guard counts bytes, so a 64-byte string of multi-byte
    /// characters reached the decode loop and panicked slicing across a
    /// character boundary. A pairing key comes off disk and off the
    /// pairing wire, so this is reachable input.
    #[test]
    fn hex_decode_rejects_non_ascii_without_panicking() {
        // One ASCII byte, then a 2-byte character, then filler: exactly
        // 64 bytes, and the character spans byte offsets 1..3. The old
        // decoder sliced `&s[0..2]`, which cuts that character in half —
        // an immediate panic on the very first iteration.
        let mut straddling = String::from("aé");
        straddling.push_str(&"a".repeat(61));
        assert_eq!(straddling.len(), 64, "must clear the byte-length guard");
        assert!(hex_decode_32(&straddling).is_none());

        // 32 x 2-byte characters is also exactly 64 bytes. Every slice
        // boundary lands cleanly here, so this one never panicked — it
        // just has to keep returning None rather than decoding garbage.
        let all_wide: String = "é".repeat(32);
        assert_eq!(all_wide.len(), 64);
        assert!(hex_decode_32(&all_wide).is_none());
    }

    #[test]
    fn hex_round_trips_and_rejects_bad_input() {
        let raw = [0xABu8; 32];
        let encoded = hex_encode_32(&raw);
        assert_eq!(encoded.len(), 64);
        assert_eq!(hex_decode_32(&encoded), Some(raw));
        // Uppercase is accepted.
        assert_eq!(hex_decode_32(&encoded.to_uppercase()), Some(raw));
        // Wrong length, and right length with a non-hex character.
        assert!(hex_decode_32("abcd").is_none());
        assert!(hex_decode_32(&"z".repeat(64)).is_none());
    }

    #[test]
    fn round_trip_through_persisted_shape() {
        let runtime = MobileSettings {
            pair_enabled: true,
            auto_connect: true,
            peerjs_broker: "0.peerjs.com".into(),
            desktop_peer_id: "DESKTOP-PEER-12345".into(),
            pairings: vec![PairingRecord {
                label: "Mike's iPhone".into(),
                phone_public_key: [7u8; 32],
                paired_at: 1_700_000_000,
                push_target: Some(PushTarget::Fcm {
                    token: "fcm-token-here".into(),
                }),
            }],
        };
        let persisted = to_persisted(
            &runtime,
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        );
        let back = from_persisted(&persisted);
        assert_eq!(back, runtime);
    }
}
