//! FFM-M21 + FFM-M24 — portable-mode status and launch-at-login.
//!
//! Both features answer the same question from opposite ends: *may this
//! install write into host-global state?* A portable install may not —
//! every autostart mechanism records an absolute path into the
//! registry / LaunchAgents / XDG autostart, and that path points at a
//! removable volume which will not be there next boot.
//!
//! So [`autostart_set`] refuses under portable mode rather than
//! registering an entry that is guaranteed to rot, and the UI shows the
//! toggle disabled with the reason.

use freally_settings::portable;
use serde::Serialize;

use crate::state::AppState;

/// Wire shape for the portable-mode banner in Settings.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableStatusDto {
    /// Whether this process is running portable.
    pub portable: bool,
    /// Where user state is being written. Absolute.
    pub data_root: String,
    /// Whether shell registration / autostart may be used.
    pub os_integration_allowed: bool,
    /// Whether the OS keychain may hold credentials. `false` under
    /// portable mode — secrets fall back to an encrypted local file,
    /// which the UI must warn about rather than silently accept.
    pub os_keychain_allowed: bool,
}

/// Wire shape for the launch-at-login toggle.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutostartStatusDto {
    /// What the **OS** currently holds, not what settings say.
    pub enabled: bool,
    /// Whether the toggle may be used at all on this install.
    pub supported: bool,
    /// Localisation key explaining why it is unavailable, when it is.
    pub reason_key: String,
}

/// `portable_status()` — where this install writes, and what it is
/// therefore allowed to do.
#[tauri::command]
pub fn portable_status() -> PortableStatusDto {
    PortableStatusDto {
        portable: portable::is_portable(),
        data_root: portable::data_root()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        os_integration_allowed: portable::allows_os_integration(),
        os_keychain_allowed: portable::allows_os_keychain(),
    }
}

/// `autostart_status()` — the live OS state of the login item.
///
/// Reads the OS rather than `Settings::general::start_with_os` so the
/// switch tells the truth after the entry was removed by hand or by a
/// startup-manager tool.
#[tauri::command]
pub fn autostart_status() -> AutostartStatusDto {
    if !portable::allows_os_integration() {
        return AutostartStatusDto {
            enabled: false,
            supported: false,
            reason_key: "settings-autostart-unavailable-portable".to_string(),
        };
    }
    match freally_platform::autostart::is_enabled() {
        Ok(enabled) => AutostartStatusDto {
            enabled,
            supported: true,
            reason_key: String::new(),
        },
        Err(e) => {
            tracing::debug!(target: "freally::autostart", error = %e, "status probe failed");
            AutostartStatusDto {
                enabled: false,
                supported: false,
                reason_key: "settings-autostart-unavailable-platform".to_string(),
            }
        }
    }
}

/// `autostart_set(enabled)` — register or deregister the login item and
/// mirror the result into settings.
///
/// Settings is written **after** the OS call succeeds, so the persisted
/// preference can never claim a login item that was never created.
#[tauri::command]
pub fn autostart_set(
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> Result<AutostartStatusDto, String> {
    autostart_set_impl(state.inner(), enabled)
}

/// Implementation of [`autostart_set`]. Public for tests.
pub fn autostart_set_impl(state: &AppState, enabled: bool) -> Result<AutostartStatusDto, String> {
    if !portable::allows_os_integration() {
        return Err("err-autostart-portable".to_string());
    }
    freally_platform::autostart::set_enabled(enabled, "Freally File Manager").map_err(|e| {
        tracing::warn!(target: "freally::autostart", error = %e, "toggle failed");
        format!("err-autostart-failed: {e}")
    })?;
    {
        let mut s = state.settings.write().unwrap_or_else(|p| p.into_inner());
        s.general.start_with_os = enabled;
        let path = state.settings_path.as_path();
        if !path.as_os_str().is_empty() {
            s.save_to(path).map_err(|e| format!("save settings: {e}"))?;
        }
    }
    Ok(autostart_status())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_normal_install_reports_itself_as_non_portable() {
        // The test binary lives under `target/`, with no marker file and
        // no env opt-in, so this is the normal-install path.
        let status = portable_status();
        assert!(!status.portable);
        assert!(status.os_integration_allowed);
        assert!(status.os_keychain_allowed);
        assert!(
            !status.data_root.is_empty(),
            "the UI needs a path to show even on a normal install",
        );
    }

    #[test]
    fn autostart_status_never_panics_and_always_explains_itself() {
        let status = autostart_status();
        if !status.supported {
            assert!(
                !status.reason_key.is_empty(),
                "an unavailable toggle must carry a reason the UI can localise",
            );
        } else {
            assert!(status.reason_key.is_empty());
        }
    }
}
