//! Phase 53 — external-editor handoff.
//!
//! The roadmap's own fallback for Office documents and anything else
//! with no in-app preview: hand the three versions to whatever the OS
//! has registered, and let the user merge there.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::Result;

/// The three files a handoff writes out for the user to open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoffSet {
    pub base: PathBuf,
    pub local: PathBuf,
    pub remote: PathBuf,
}

/// Suffix a path before its extension.
///
/// `report.docx` → `report.local.docx`, not `report.docx.local` — the
/// extension has to survive or the OS will not know what opens it,
/// which defeats the entire point of a handoff.
pub fn suffixed(path: &Path, tag: &str) -> PathBuf {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let name = match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{stem}.{tag}.{ext}"),
        None => format!("{stem}.{tag}"),
    };
    path.with_file_name(name)
}

/// Write the three versions beside `target` for external merging.
pub fn write_set(target: &Path, base: &[u8], local: &[u8], remote: &[u8]) -> Result<HandoffSet> {
    let set = HandoffSet {
        base: suffixed(target, "base"),
        local: suffixed(target, "local"),
        remote: suffixed(target, "remote"),
    };
    std::fs::write(&set.base, base)?;
    std::fs::write(&set.local, local)?;
    std::fs::write(&set.remote, remote)?;
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_extension_survives_so_the_os_still_knows_what_opens_it() {
        let p = Path::new("/docs/report.docx");
        assert_eq!(suffixed(p, "local"), Path::new("/docs/report.local.docx"));
        assert_eq!(suffixed(p, "base"), Path::new("/docs/report.base.docx"));
    }

    #[test]
    fn extensionless_and_dotted_names_are_handled() {
        assert_eq!(
            suffixed(Path::new("/x/README"), "remote"),
            Path::new("/x/README.remote")
        );
        // Only the final extension is the extension.
        assert_eq!(
            suffixed(Path::new("/x/archive.tar.gz"), "local"),
            Path::new("/x/archive.tar.local.gz")
        );
    }

    #[test]
    fn write_set_lays_down_all_three_versions() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sheet.xlsx");
        let set = write_set(&target, b"BASE", b"LOCAL", b"REMOTE").unwrap();

        assert_eq!(std::fs::read(&set.base).unwrap(), b"BASE");
        assert_eq!(std::fs::read(&set.local).unwrap(), b"LOCAL");
        assert_eq!(std::fs::read(&set.remote).unwrap(), b"REMOTE");
        // The original is untouched — a handoff must never overwrite the
        // file the user is still deciding about.
        assert!(!target.exists());
    }
}
