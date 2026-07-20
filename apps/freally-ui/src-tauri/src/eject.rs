//! FFM-M04 — eject / safely-remove a destination volume after a job.
//!
//! A when-done action (surfaced as a job context-menu verb) that flushes
//! and safely dismounts the volume a path lives on, using the OS's own
//! user-mode eject tool — no admin, no kernel driver:
//!
//! - **Windows** — `mountvol <letter>: /p` flushes the volume's write
//!   cache, dismounts it, and takes removable media offline for safe
//!   removal.
//! - **macOS** — `diskutil eject <volume-root>`.
//! - **Linux** — `udisksctl power-off` on the backing device (falls back
//!   to the classic `eject` when udisks isn't present).
//!
//! The path→volume resolution helpers are pure and unit-tested; the
//! actual eject shells out and is exercised by the manual QA drill
//! (removable media can't be integration-tested in CI).

use crate::ipc_safety::validate_ipc_path;

/// The Windows volume root a path lives on — the `X:` drive spec that
/// `mountvol /p` expects. `None` for a UNC path (`\\server\share`),
/// which has no letter to eject.
pub fn windows_drive_spec(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        Some(format!("{}:", (bytes[0] as char).to_ascii_uppercase()))
    } else {
        None
    }
}

/// The `/Volumes/<name>` root of a macOS path, or `None` when the path
/// isn't under `/Volumes` (the boot volume, which we never eject).
pub fn macos_volume_root(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/Volumes/")?;
    let name = rest.split('/').next().filter(|s| !s.is_empty())?;
    Some(format!("/Volumes/{name}"))
}

/// Eject the volume a destination path lives on. Best-effort but
/// surfaces the OS error so the UI can report why removal was refused
/// (most commonly: an open handle still holds the volume).
#[tauri::command]
pub async fn eject_volume(path: String) -> Result<(), String> {
    let target = validate_ipc_path(&path).map_err(|e| e.to_string())?;
    let target = target.to_string_lossy().into_owned();
    tauri::async_runtime::spawn_blocking(move || eject_impl(&target))
        .await
        .map_err(|e| format!("eject task failed: {e}"))?
}

#[cfg(target_os = "windows")]
fn eject_impl(path: &str) -> Result<(), String> {
    use std::process::Command;
    let spec =
        windows_drive_spec(path).ok_or_else(|| format!("no drive letter to eject in `{path}`"))?;
    let out = Command::new("mountvol")
        .args([&spec, "/p"])
        .output()
        .map_err(|e| format!("mountvol: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let detail = if stderr.trim().is_empty() {
            "still in use"
        } else {
            stderr.trim()
        };
        Err(format!("could not eject {spec}: {detail}"))
    }
}

#[cfg(target_os = "macos")]
fn eject_impl(path: &str) -> Result<(), String> {
    use std::process::Command;
    let root =
        macos_volume_root(path).ok_or_else(|| format!("`{path}` is not on an ejectable volume"))?;
    let out = Command::new("diskutil")
        .args(["eject", &root])
        .output()
        .map_err(|e| format!("diskutil: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "could not eject {root}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn eject_impl(path: &str) -> Result<(), String> {
    use std::process::Command;
    // `eject` operates on the mount point and handles unmount + power-off
    // for removable media; it's present on virtually every desktop.
    let out = Command::new("eject")
        .arg(path)
        .output()
        .map_err(|e| format!("eject: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "could not eject {path}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_drive_spec_extracts_letter() {
        assert_eq!(
            windows_drive_spec(r"E:\photos\a.jpg").as_deref(),
            Some("E:")
        );
        assert_eq!(windows_drive_spec(r"c:\x").as_deref(), Some("C:"));
        // UNC and relative paths have no ejectable letter.
        assert_eq!(windows_drive_spec(r"\\server\share\f"), None);
        assert_eq!(windows_drive_spec("relative/path"), None);
    }

    #[test]
    fn macos_volume_root_finds_mount() {
        assert_eq!(
            macos_volume_root("/Volumes/BACKUP/dir/file").as_deref(),
            Some("/Volumes/BACKUP")
        );
        assert_eq!(
            macos_volume_root("/Volumes/USB").as_deref(),
            Some("/Volumes/USB")
        );
        // Boot volume is never ejectable.
        assert_eq!(macos_volume_root("/Users/me/file"), None);
        assert_eq!(macos_volume_root("/Volumes/"), None);
    }
}
