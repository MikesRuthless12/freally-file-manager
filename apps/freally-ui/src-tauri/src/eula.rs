//! First-run EULA acceptance gate.
//!
//! The full agreement lives in the repo-root `EULA.md` (embedded here at
//! build time so the text the user accepts is exactly the text this build
//! ships). The app does not render its main UI until the user accepts the
//! current [`EULA_VERSION`]; acceptance is persisted in settings. Bump
//! [`EULA_VERSION`] whenever `EULA.md` changes in a way that requires
//! re-acceptance — a stale accepted version re-shows the gate.

use serde::Serialize;
use tauri::State;

use crate::state::AppState;

/// The agreement text, embedded from the repo-root `EULA.md`.
pub const EULA_TEXT: &str = include_str!("../../../../EULA.md");

/// The current EULA version. Bump on any material `EULA.md` change so users
/// are asked to accept the new terms. (Date-stamped for a human-readable
/// audit.)
pub const EULA_VERSION: &str = "2026-07-19";

/// What the UI needs to render + gate on the EULA.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EulaStatus {
    /// The current EULA version.
    pub version: String,
    /// The full agreement text (markdown).
    pub text: String,
    /// Whether the user has already accepted this exact version.
    pub accepted: bool,
}

/// The EULA status: the text + whether the current version is accepted.
#[tauri::command]
pub fn eula_status(state: State<'_, AppState>) -> EulaStatus {
    let s = state.settings_snapshot();
    EulaStatus {
        version: EULA_VERSION.to_string(),
        text: EULA_TEXT.to_string(),
        accepted: s.eula.accepted_version.as_deref() == Some(EULA_VERSION),
    }
}

/// Record acceptance of the current EULA version (persisted). Idempotent.
///
/// Persist-first like `update_settings`: if the disk write fails the
/// in-memory state is left untouched and the gate stays up rather than
/// lying that acceptance was recorded. Only the `eula` group is mutated —
/// `update_settings` separately carries the group across wholesale DTO
/// replaces, so acceptance survives every later settings save.
#[tauri::command]
pub fn eula_accept(state: State<'_, AppState>) -> Result<(), String> {
    let mut next = state.settings_snapshot();
    next.eula.accepted_version = Some(EULA_VERSION.to_string());
    let path = state.settings_path.as_ref();
    if !path.as_os_str().is_empty() {
        next.save_to(path)
            .map_err(|e| format!("could not record EULA acceptance: {e}"))?;
    }
    let mut live = state
        .settings
        .write()
        .map_err(|_| "settings-lock-poisoned".to_string())?;
    live.eula = next.eula;
    Ok(())
}

/// Decline the EULA: exit without recording anything. A dedicated
/// command (rather than `tauri-plugin-process` on the frontend) so the
/// only quit path from the gate is auditable here.
#[tauri::command]
pub fn eula_decline_quit(app: tauri::AppHandle) {
    app.exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eula_text_is_embedded_and_versioned() {
        assert!(EULA_TEXT.contains("End User License Agreement"));
        // A material clause the acceptance is meant to bind the user to.
        assert!(EULA_TEXT.contains("solely responsible"));
        assert_eq!(EULA_VERSION.split('-').count(), 3, "date-stamped version");
    }

    /// EulaGate.svelte renders #/## headings, `**bold**`, `*italic*`,
    /// `` `code` ``, -/* lists, and > quotes (bold/italic spans may wrap
    /// across the source's hard line breaks); any other markdown renders
    /// literally in the accept dialog. Pin `EULA.md` to that subset so a
    /// routine legal edit can't ship a garbled agreement.
    #[test]
    fn eula_text_stays_within_the_gate_renderer_subset() {
        for line in EULA_TEXT.lines() {
            let t = line.trim_start();
            assert!(!t.contains("]("), "markdown links unsupported: {line:?}");
            assert!(!t.starts_with("```"), "code fences unsupported: {line:?}");
            assert!(!t.contains('|'), "tables unsupported: {line:?}");
            assert!(
                !(t == "---" || t == "***"),
                "horizontal rules unsupported: {line:?}"
            );
            let numbered = t.split_once(". ").is_some_and(|(head, _)| {
                !head.is_empty() && head.bytes().all(|b| b.is_ascii_digit())
            });
            assert!(!numbered, "numbered lists unsupported: {line:?}");
        }
        // Balanced emphasis markers: an odd count leaves a stray `*` /
        // `**` rendering as a literal asterisk mid-sentence.
        assert_eq!(
            EULA_TEXT.matches("**").count() % 2,
            0,
            "unbalanced ** bold markers in EULA.md"
        );
        assert_eq!(
            EULA_TEXT.replace("**", "").matches('*').count() % 2,
            0,
            "unbalanced * italic markers in EULA.md"
        );
    }
}
