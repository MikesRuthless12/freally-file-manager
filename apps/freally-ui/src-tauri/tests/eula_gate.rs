//! Integration tests for the first-run EULA gate's persistence
//! semantics — the pieces that work without booting a webview (the
//! same discipline as `command_layer.rs`): the acceptance round-trip
//! through `Settings` save/load, the version-bump re-prompt, and the
//! `update_settings` carry-over that keeps a wholesale DTO replace
//! from clearing a recorded acceptance.
//!
//! The visual half of the acceptance test (gate blocks the main UI →
//! Agree → app opens) is a Playwright/human drill; see
//! `Live-To-Do-List.md`.

use freally_settings::Settings;
use freally_ui_lib::eula::EULA_VERSION;
use freally_ui_lib::ipc::SettingsDto;
use tempfile::tempdir;

/// Fresh install: no acceptance recorded → the gate must show.
#[test]
fn fresh_settings_are_unaccepted() {
    let s = Settings::default();
    assert_ne!(s.eula.accepted_version.as_deref(), Some(EULA_VERSION));
}

/// Accept → persist → reload: acceptance survives a relaunch, so the
/// gate never re-prompts for the same version.
#[test]
fn acceptance_survives_relaunch() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    let mut s = Settings::default();
    s.eula.accepted_version = Some(EULA_VERSION.to_string());
    s.save_to(&path).unwrap();
    let reloaded = Settings::load_from(&path).unwrap();
    assert_eq!(
        reloaded.eula.accepted_version.as_deref(),
        Some(EULA_VERSION)
    );
}

/// The SettingsDto deliberately does not carry the EULA group — a
/// frontend-authored wholesale replace would otherwise reset it. This
/// pins the DTO half of the contract; the restoring half is the
/// carry-over in `commands.rs::update_settings` (and the matching
/// preserves in `reset_settings` / `load_profile` / `save_profile`).
#[test]
fn settings_dto_never_carries_the_eula_group() {
    let mut prev = Settings::default();
    prev.eula.accepted_version = Some(EULA_VERSION.to_string());
    let dto: SettingsDto = (&prev).into();
    let next = dto.into_settings();
    assert!(
        next.eula.accepted_version.is_none(),
        "SettingsDto must not carry the EULA group"
    );
}
