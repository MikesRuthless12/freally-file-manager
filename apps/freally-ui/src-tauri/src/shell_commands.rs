//! FFM-M01 — Explorer copy-verb takeover control surface.
//!
//! The stored [`freally_settings::ShellSettings::intercept_default_copy`]
//! flag now drives a real registry override: when on, Explorer's built-in
//! copy verb (Ctrl+C / Ctrl+V / drag-copy) is rerouted into Freally's
//! queue via `HKCU\…\*\shell\copy\DelegateExecute`. `update_settings`
//! calls [`sync_copy_interceptor`] whenever the flag flips; this module
//! also exposes a status probe and a one-click revert command.
//!
//! Windows-only: Finder and GTK expose no equivalent copy-verb hook, so
//! on other platforms every entry point is a benign no-op and the status
//! reports `supported: false`.

use serde::Serialize;
use tauri::State;

use crate::state::AppState;

/// What the Settings UI needs to reflect the true interceptor state —
/// which can drift from the stored flag (the handler DLL may not be
/// registered, or another tool may have cleared the override).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyInterceptStatus {
    /// The platform can intercept the OS copy verb at all (Windows only).
    pub supported: bool,
    /// The Freally copy handler is class-registered — i.e. context-menu
    /// integration is installed. Interception cannot arm without it.
    pub handler_registered: bool,
    /// Our override is currently the active copy handler.
    pub active: bool,
}

/// Apply or revert the copy interceptor to match `enabled`.
///
/// Returns the OS error message on failure (most commonly: the copy
/// handler DLL is not registered, so arming is refused to avoid breaking
/// the user's Ctrl+C). No-op success on non-Windows.
#[cfg(windows)]
pub fn sync_copy_interceptor(enabled: bool) -> Result<(), String> {
    use freally_shellext::registry;
    let res = if enabled {
        registry::apply_copy_interceptor()
    } else {
        registry::remove_copy_interceptor()
    };
    res.map_err(|e| e.message())
}

#[cfg(not(windows))]
pub fn sync_copy_interceptor(_enabled: bool) -> Result<(), String> {
    Ok(())
}

/// Locate the shell-extension DLL that ships beside the app (packaged)
/// or in the cargo target dir (dev). The registration writes this path
/// into `InprocServer32` so Explorer can load the handler.
#[cfg(windows)]
fn shellext_dll_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [
        dir.join("freally_shellext.dll"),
        dir.join("deps").join("freally_shellext.dll"),
    ]
    .into_iter()
    .find(|p| p.exists())
}

/// Register or unregister the Explorer context-menu handler — the
/// actual `HKCU\…\CLSID\…` + verb keys, done from inside the app so the
/// Settings checkbox controls the real registry state (not just a
/// stored preference). Unregistering also removes the copy interceptor,
/// which would otherwise point Explorer's copy verb at a class we just
/// deleted. No-op success on non-Windows (Finder/GTK integration is
/// installer-managed there).
#[cfg(windows)]
pub fn sync_context_menu_registration(enabled: bool) -> Result<(), String> {
    use freally_shellext::registry::{self, InstallScope};
    if enabled {
        let dll = shellext_dll_path()
            .ok_or("the Freally shell-extension DLL was not found next to the app")?;
        let keys = registry::all_registration_keys(InstallScope::PerUser, &dll.to_string_lossy());
        registry::apply_registration(&keys).map_err(|e| e.message())
    } else {
        // Drop the interceptor first — it references the CLSID below.
        let _ = registry::remove_copy_interceptor();
        // The delete path only reads key names, so an empty DLL arg is
        // fine for composing the paths to remove.
        let keys = registry::all_registration_keys(InstallScope::PerUser, "");
        registry::delete_registration(&keys).map_err(|e| e.message())
    }
}

#[cfg(not(windows))]
pub fn sync_context_menu_registration(_enabled: bool) -> Result<(), String> {
    Ok(())
}

/// Probe the live interceptor state for the Settings UI.
#[tauri::command]
pub fn shell_copy_intercept_status() -> CopyInterceptStatus {
    #[cfg(windows)]
    {
        use freally_shellext::consts::CLSID_COPY_STR;
        use freally_shellext::registry;
        CopyInterceptStatus {
            supported: true,
            handler_registered: registry::clsid_registered(CLSID_COPY_STR),
            active: registry::copy_interceptor_active(),
        }
    }
    #[cfg(not(windows))]
    {
        CopyInterceptStatus {
            supported: false,
            handler_registered: false,
            active: false,
        }
    }
}

// ---------------------------------------------------------------------
// FFM-M01 — paste chooser "System copy / move".
// ---------------------------------------------------------------------

/// What [`system_paste`] returns for the completion toast. `items` are
/// files copied plus whole entries moved by rename (a same-volume
/// renamed folder counts once, with 0 bytes — nothing was rewritten).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemPasteReport {
    pub items: u64,
    pub bytes: u64,
}

/// Plain unverified recursive transfer — the deliberate contrast to
/// Freally's verified engine: no hashing, no journal, no queue, like
/// the OS file manager's own paste. Overwrites existing files, exactly
/// as an OS paste does after its confirm.
pub(crate) fn plain_transfer(
    src: &std::path::Path,
    target: &std::path::Path,
    report: &mut SystemPasteReport,
) -> Result<(), String> {
    if src.is_dir() {
        // Pasting a folder into itself would recurse forever.
        if target.starts_with(src) {
            return Err(format!(
                "cannot paste a folder into itself: {}",
                src.display()
            ));
        }
        std::fs::create_dir_all(target).map_err(|e| format!("{}: {e}", target.display()))?;
        let entries = std::fs::read_dir(src).map_err(|e| format!("{}: {e}", src.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("{}: {e}", src.display()))?;
            plain_transfer(&entry.path(), &target.join(entry.file_name()), report)?;
        }
    } else {
        let bytes = std::fs::copy(src, target).map_err(|e| format!("{}: {e}", src.display()))?;
        report.items += 1;
        report.bytes += bytes;
    }
    Ok(())
}

pub(crate) fn remove_source(src: &std::path::Path) -> Result<(), String> {
    let res = if src.is_dir() {
        std::fs::remove_dir_all(src)
    } else {
        std::fs::remove_file(src)
    };
    res.map_err(|e| format!("{}: {e}", src.display()))
}

/// The paste chooser's "System copy / move" row: a fast, unverified
/// filesystem transfer on a blocking thread. `verb` is `"copy"` or
/// `"move"`; move tries a same-volume rename first and falls back to
/// copy + delete across volumes.
#[tauri::command]
pub async fn system_paste(
    verb: String,
    sources: Vec<String>,
    destination: String,
) -> Result<SystemPasteReport, String> {
    // Route every path through the IPC gate, matching every sibling
    // command (trash / eject / sidecar / export). Defense-in-depth: it
    // rejects U+FFFD-mangled, NUL/control-char, and `..` payloads before
    // any filesystem op.
    let dest = crate::ipc_safety::validate_ipc_path(&destination).map_err(|e| e.to_string())?;
    let mut srcs = Vec::with_capacity(sources.len());
    for s in &sources {
        srcs.push(crate::ipc_safety::validate_ipc_path(s).map_err(|e| e.to_string())?);
    }
    tauri::async_runtime::spawn_blocking(move || {
        if !dest.is_dir() {
            return Err(format!("destination is not a folder: {}", dest.display()));
        }
        let mut report = SystemPasteReport::default();
        for src in &srcs {
            let name = src
                .file_name()
                .ok_or_else(|| format!("source has no file name: {}", src.display()))?;
            let target = dest.join(name);
            if verb == "move" && std::fs::rename(src, &target).is_ok() {
                report.items += 1;
                continue;
            }
            plain_transfer(src, &target, &mut report)?;
            if verb == "move" {
                remove_source(src)?;
            }
        }
        Ok(report)
    })
    .await
    .map_err(|e| format!("system paste task failed: {e}"))?
}

/// One-click revert to the OS copy handler: remove the override and clear
/// the stored flag so the two stay consistent. Idempotent.
#[tauri::command]
pub fn shell_revert_os_copy_handler(state: State<'_, AppState>) -> Result<(), String> {
    sync_copy_interceptor(false)?;
    // Persist-first, then swap the live flag — mirrors `eula_accept`.
    let mut next = state.settings_snapshot();
    next.shell.intercept_default_copy = false;
    let path = state.settings_path.as_ref();
    if !path.as_os_str().is_empty() {
        next.save_to(path).map_err(|e| e.to_string())?;
    }
    state
        .settings
        .write()
        .map_err(|_| "settings-lock-poisoned".to_string())?
        .shell
        .intercept_default_copy = false;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_transfer_copies_a_tree_and_counts_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("nested")).unwrap();
        std::fs::write(src.join("a.txt"), b"hello").unwrap();
        std::fs::write(src.join("nested").join("b.txt"), b"world!!").unwrap();

        let mut report = SystemPasteReport::default();
        let target = dir.path().join("dst").join("src");
        plain_transfer(&src, &target, &mut report).unwrap();

        assert_eq!(report.items, 2);
        assert_eq!(report.bytes, 12);
        assert_eq!(std::fs::read(target.join("a.txt")).unwrap(), b"hello");
        assert_eq!(
            std::fs::read(target.join("nested").join("b.txt")).unwrap(),
            b"world!!"
        );
    }

    #[test]
    fn plain_transfer_overwrites_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.txt");
        let dst = dir.path().join("out.txt");
        std::fs::write(&src, b"new-content").unwrap();
        std::fs::write(&dst, b"old").unwrap();

        let mut report = SystemPasteReport::default();
        plain_transfer(&src, &dst, &mut report).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"new-content");
    }

    #[test]
    fn plain_transfer_refuses_folder_into_itself() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("folder");
        std::fs::create_dir_all(&src).unwrap();
        let mut report = SystemPasteReport::default();
        let err = plain_transfer(&src, &src.join("folder"), &mut report).unwrap_err();
        assert!(err.contains("into itself"), "{err}");
    }

    #[test]
    fn remove_source_handles_files_and_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f.txt");
        std::fs::write(&f, b"x").unwrap();
        remove_source(&f).unwrap();
        assert!(!f.exists());

        let d = dir.path().join("d");
        std::fs::create_dir_all(d.join("inner")).unwrap();
        remove_source(&d).unwrap();
        assert!(!d.exists());
    }
}
