//! Phase 51 — end-to-end encrypted collaboration IPC.
//!
//! Exposes [`freally_collab`]'s roster, envelope crypto, signed
//! revocation, and SAS verification to the frontend.
//!
//! # Where the roster and admin key live
//!
//! The roster is public metadata (labels + X25519 public keys) and
//! lives beside `settings.toml` as `collab-roster.json`. The **admin
//! signing seed** is a secret — anyone holding it can forge a
//! revocation — so it goes through the same credential store cloud
//! backends use: an age-encrypted file on a portable drive (FFM-M21),
//! the OS keychain otherwise. It is never written to the roster file.
//!
//! # What this does not do
//!
//! Revocation is forward-only: removing a member changes who can read
//! files encrypted *afterwards* and cannot claw back what they already
//! had. The UI copy says so, and `freally_collab`'s crate docs explain
//! why a truthful implementation is impossible.

use std::path::PathBuf;

use freally_collab::{AdminKey, Roster};
use serde::Serialize;

use crate::state::AppState;

/// Keychain/credential-store entry holding the admin signing seed.
///
/// Deliberately namespaced away from the cloud backend names so a
/// backend called `collab` cannot collide with it.
const ADMIN_KEY_ENTRY: &str = "collab-admin-key";

/// `<config-root>/collab-roster.json`.
fn roster_path(state: &AppState) -> PathBuf {
    let settings_path = state.settings_path.as_path();
    settings_path
        .parent()
        .map(|d| d.join("collab-roster.json"))
        .unwrap_or_else(|| PathBuf::from("collab-roster.json"))
}

fn load_roster(state: &AppState) -> Result<Option<Roster>, String> {
    let path = roster_path(state);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let roster = Roster::from_json(&text).map_err(|e| format!("roster: {e}"))?;
            // This is trusted local state, not authenticated peer input.
            // `verify_revocations` checks tokens against the roster's own
            // `admin_public`, so it proves internal consistency rather
            // than origin, and nothing signs `members` at all. It catches
            // a truncated or hand-edited file, not a hostile one — and it
            // does not need to: the roster lives in the per-user config
            // dir, where anyone who can write it can already load a wasm
            // plugin with self-granted capabilities.
            //
            // There is no peer-import path today. If one is ever added,
            // the roster body needs its own signature and `admin_public`
            // must be pinned against the credential store's key first.
            roster
                .verify_revocations()
                .map_err(|e| format!("roster: {e}"))?;
            Ok(Some(roster))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("roster: {e}")),
    }
}

fn save_roster(state: &AppState, roster: &Roster) -> Result<(), String> {
    let path = roster_path(state);
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| format!("roster: {e}"))?;
    }
    let json = roster.to_json().map_err(|e| format!("roster: {e}"))?;
    // Stage + rename. A crash partway through a plain write leaves a
    // truncated roster, and `load_roster` hard-errors on malformed
    // JSON — which would take the whole collaboration panel down with
    // no way back. Matches the tmp+rename in `plugin_commands` and
    // `dropstack`.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("roster: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("roster: {e}"))
}

/// Load the admin key from the credential store, or mint and store one.
fn admin_key(state: &AppState) -> Result<AdminKey, String> {
    let store = crate::cloud_commands::credential_store(state)?;
    if let Some(hex) = store.load(ADMIN_KEY_ENTRY).map_err(|e| e.to_string())? {
        let raw = hex_to_seed(&hex).ok_or("collab-admin-key-corrupt")?;
        return Ok(AdminKey::from_seed(&raw));
    }
    let key = AdminKey::generate().map_err(|e| e.to_string())?;
    store
        .store(ADMIN_KEY_ENTRY, &seed_to_hex(&key.seed()))
        .map_err(|e| e.to_string())?;
    Ok(key)
}

/// The admin key, checked against the one the roster was created with.
///
/// The roster lives beside `settings.toml` while the seed lives in the
/// credential store, and those two can part company — a restored config
/// dir, a new machine, or toggling portable mode (which swaps the OS
/// keychain for the encrypted file). `admin_key` would then mint a
/// *fresh* key, sign a revocation with it, and save a roster whose
/// tokens no longer verify against its own `admin_public`; from that
/// point `load_roster` hard-errors and every collab command, including
/// read-only `collab_roster`, fails permanently with no way back.
/// Refusing up front leaves the roster intact.
fn admin_key_for(state: &AppState, roster: &Roster) -> Result<AdminKey, String> {
    let key = admin_key(state)?;
    if seed_to_hex(&key.public()) != roster.admin_public {
        return Err(
            "this install's collaboration admin key does not match the roster's; \
             restore the original key before changing membership"
                .to_string(),
        );
    }
    Ok(key)
}

fn seed_to_hex(seed: &[u8; 32]) -> String {
    seed.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_to_seed(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// One member, as the panel renders it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollabMemberDto {
    pub label: String,
    /// `age1…` public key. Public by design — it is what a peer needs.
    pub recipient: String,
}

/// The collaboration panel's whole state.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollabRosterDto {
    /// Whether a roster exists yet.
    pub initialized: bool,
    pub epoch: u64,
    pub members: Vec<CollabMemberDto>,
    /// Labels revoked so far, newest last.
    pub revoked: Vec<String>,
}

/// `collab_roster()` — the current roster, or an empty one.
#[tauri::command]
pub fn collab_roster(state: tauri::State<'_, AppState>) -> Result<CollabRosterDto, String> {
    let Some(roster) = load_roster(&state)? else {
        return Ok(CollabRosterDto {
            initialized: false,
            epoch: 0,
            members: Vec::new(),
            revoked: Vec::new(),
        });
    };
    Ok(CollabRosterDto {
        initialized: true,
        epoch: roster.epoch,
        members: roster
            .members
            .iter()
            .map(|m| CollabMemberDto {
                label: m.label.clone(),
                recipient: m.recipient.clone(),
            })
            .collect(),
        revoked: roster.revocations.iter().map(|r| r.label.clone()).collect(),
    })
}

/// `collab_add_member(label, recipient)` — add a collaborator.
///
/// Creates the roster (and the admin key) on first use.
#[tauri::command]
pub fn collab_add_member(
    state: tauri::State<'_, AppState>,
    label: String,
    recipient: String,
) -> Result<(), String> {
    let admin = admin_key(&state)?;
    let mut roster = match load_roster(&state)? {
        Some(r) => r,
        None => Roster::new(&admin),
    };
    roster
        .add_member(&label, &recipient)
        .map_err(|e| e.to_string())?;
    save_roster(&state, &roster)
}

/// `collab_remove_member(label)` — revoke a collaborator.
///
/// Forward-only: this changes who can read files encrypted after it and
/// cannot claw back what the member already had.
#[tauri::command]
pub fn collab_remove_member(
    state: tauri::State<'_, AppState>,
    label: String,
) -> Result<(), String> {
    let mut roster = load_roster(&state)?.ok_or("collab-no-roster")?;
    let admin = admin_key_for(&state, &roster)?;
    roster
        .remove_member(&label, &admin)
        .map_err(|e| e.to_string())?;
    save_roster(&state, &roster)
}

/// `collab_generate_identity()` — mint a keypair for a new member.
///
/// Returns `[secret, recipient]`. The secret is shown **once** for the
/// member to store; nothing here persists it.
#[tauri::command]
pub fn collab_generate_identity() -> Vec<String> {
    let (secret, recipient) = freally_collab::generate_member();
    vec![secret, recipient]
}

/// `collab_sas(a, b)` — the short code two people read to each other to
/// confirm they hold each other's real keys.
///
/// Order-independent, so neither side has to agree an ordering first.
#[tauri::command]
pub fn collab_sas(a: String, b: String) -> String {
    freally_collab::sas(&a, &b)
}

/// `collab_encrypt(path, dst)` — encrypt a file to every member.
#[tauri::command]
pub fn collab_encrypt(
    state: tauri::State<'_, AppState>,
    path: String,
    dst: String,
) -> Result<(), String> {
    let src = crate::ipc_safety::validate_ipc_path(&path).map_err(|e| e.to_string())?;
    let out = crate::ipc_safety::validate_ipc_path(&dst).map_err(|e| e.to_string())?;
    let roster = load_roster(&state)?.ok_or("collab-no-roster")?;
    let plaintext = std::fs::read(&src).map_err(|e| format!("read {}: {e}", src.display()))?;
    let sealed = freally_collab::encrypt_for(&roster, &plaintext).map_err(|e| e.to_string())?;
    std::fs::write(&out, sealed).map_err(|e| format!("write {}: {e}", out.display()))
}

/// `collab_decrypt(path, dst, secret)` — decrypt with your own identity.
#[tauri::command]
pub fn collab_decrypt(path: String, dst: String, secret: String) -> Result<(), String> {
    let src = crate::ipc_safety::validate_ipc_path(&path).map_err(|e| e.to_string())?;
    let out = crate::ipc_safety::validate_ipc_path(&dst).map_err(|e| e.to_string())?;
    let sealed = std::fs::read(&src).map_err(|e| format!("read {}: {e}", src.display()))?;
    let plaintext = freally_collab::decrypt_with(&secret, &sealed).map_err(|e| e.to_string())?;
    std::fs::write(&out, plaintext).map_err(|e| format!("write {}: {e}", out.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_hex_round_trips() {
        let seed = [7u8; 32];
        let hex = seed_to_hex(&seed);
        assert_eq!(hex.len(), 64);
        assert_eq!(hex_to_seed(&hex), Some(seed));
    }

    #[test]
    fn corrupt_seed_hex_is_rejected_rather_than_padded() {
        // Silently accepting a short seed would derive a *different*
        // admin key and invalidate every existing revocation.
        assert_eq!(hex_to_seed("abcd"), None);
        assert_eq!(hex_to_seed(&"z".repeat(64)), None);
    }
}
