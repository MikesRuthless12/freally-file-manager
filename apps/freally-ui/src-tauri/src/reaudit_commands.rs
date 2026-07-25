//! FFM-M11 — verify-only re-audit jobs.
//!
//! Re-hash a source tree and a destination tree and report the drift
//! between them, writing nothing. Distinct from the two verify paths
//! that already exist: Phase 43 provenance verify needs a manifest
//! written at copy time, and the FFM-M08 sidecar verify checks files
//! against a checksum file. This needs neither — hand it any two paths
//! (or press "Re-verify" on a history row, which hands it that job's
//! recorded source and destination) and it answers "are these still
//! the same?".
//!
//! Size is compared first, so a file whose length already differs is
//! reported without either side being hashed. Only *problem* rows
//! travel over IPC — the counts stay exact.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::State;

use freally_hash::HashAlgorithm;
use freally_history::{ItemRow, JobSummary};

use crate::ipc_safety::validate_ipc_path;
use crate::sidecar_commands::rel_display;
use crate::state::AppState;

/// Keep the transported problem list bounded so a wholly-diverged pair
/// of trees can't blow up the IPC payload. The counts stay exact.
const MAX_ROWS: usize = 1000;

/// One drifted file. `rel` is relative to each tree's own root, so the
/// same string names both sides.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReauditRowDto {
    pub rel: String,
    /// `"differs"` | `"missing"` (absent at the destination) |
    /// `"extra"` (present only at the destination) | `"error"`.
    pub status: String,
    /// Why: `"size"` / `"hash"` for a difference, else the OS error.
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReauditReport {
    pub algo: String,
    pub src_root: String,
    pub dst_root: String,
    /// Files present on both sides with identical content.
    pub ok: u64,
    pub differs: u64,
    pub missing: u64,
    pub extra: u64,
    pub errors: u64,
    /// The drifted rows only (bounded by [`MAX_ROWS`]); matching files
    /// are counted, not listed.
    pub rows: Vec<ReauditRowDto>,
}

impl ReauditReport {
    fn clean(&self) -> bool {
        self.differs == 0 && self.missing == 0 && self.extra == 0 && self.errors == 0
    }

    fn push(&mut self, rel: &Path, status: &str, detail: Option<String>) {
        if self.rows.len() < MAX_ROWS {
            self.rows.push(ReauditRowDto {
                rel: rel_display(rel),
                status: status.to_string(),
                detail,
            });
        }
    }
}

/// The directory relative paths are anchored at, plus every file under
/// `root`. A single-file root anchors at its parent so `src`/`dst`
/// pairs that name one file each still line up on the same `rel`.
fn walk_root(root: &Path) -> std::io::Result<(PathBuf, BTreeSet<PathBuf>)> {
    let rels = crate::sidecar_commands::collect_files(root)?;
    let anchor = if root.is_dir() {
        root.to_path_buf()
    } else {
        root.parent().map(Path::to_path_buf).unwrap_or_default()
    };
    Ok((anchor, rels.into_iter().collect()))
}

/// Re-hash `src` and `dst` and report the drift. Writes nothing.
#[tauri::command]
pub async fn reaudit_run(
    src: String,
    dst: String,
    algo: String,
    state: State<'_, AppState>,
) -> Result<ReauditReport, String> {
    use std::str::FromStr;
    let src = validate_ipc_path(&src).map_err(|e| e.to_string())?;
    let dst = validate_ipc_path(&dst).map_err(|e| e.to_string())?;
    let algo = HashAlgorithm::from_str(&algo).map_err(|_| format!("unknown algorithm: {algo}"))?;

    let (src_anchor, src_rels, dst_anchor, dst_rels) = {
        let (src, dst) = (src.clone(), dst.clone());
        tauri::async_runtime::spawn_blocking(move || -> Result<_, String> {
            let (src_anchor, src_rels) =
                walk_root(&src).map_err(|e| format!("{}: {e}", src.display()))?;
            let (dst_anchor, dst_rels) =
                walk_root(&dst).map_err(|e| format!("{}: {e}", dst.display()))?;
            Ok((src_anchor, src_rels, dst_anchor, dst_rels))
        })
        .await
        .map_err(|e| format!("re-audit walk failed: {e}"))??
    };

    let mut report = ReauditReport {
        algo: algo.name().to_string(),
        src_root: src.to_string_lossy().into_owned(),
        dst_root: dst.to_string_lossy().into_owned(),
        ..Default::default()
    };

    for rel in &src_rels {
        if !dst_rels.contains(rel) {
            report.missing += 1;
            report.push(rel, "missing", None);
            continue;
        }
        let src_path = src_anchor.join(rel);
        let dst_path = dst_anchor.join(rel);
        match compare_sizes(&src_path, &dst_path).await {
            Err(e) => {
                report.errors += 1;
                report.push(rel, "error", Some(e));
                continue;
            }
            // A length mismatch is already proof of drift — never hash
            // gigabytes to confirm what `stat` settled.
            Ok(false) => {
                report.differs += 1;
                report.push(rel, "differs", Some("size".to_string()));
                continue;
            }
            Ok(true) => {}
        }
        // Both sides are independent reads and usually sit on different
        // devices (that is the whole point of auditing a copy), so let
        // the two I/O streams overlap.
        let (src_hex, dst_hex) = tokio::join!(
            crate::hashing::hash_file_hex(&src_path, algo),
            crate::hashing::hash_file_hex(&dst_path, algo),
        );
        match (src_hex, dst_hex) {
            (Ok(a), Ok(b)) if a == b => report.ok += 1,
            (Ok(_), Ok(_)) => {
                report.differs += 1;
                report.push(rel, "differs", Some("hash".to_string()));
            }
            (Err(e), _) | (_, Err(e)) => {
                report.errors += 1;
                report.push(rel, "error", Some(e));
            }
        }
    }

    for rel in dst_rels.difference(&src_rels) {
        report.extra += 1;
        report.push(rel, "extra", None);
    }

    record_history(&state, &src, &dst, &report).await;
    Ok(report)
}

/// `Ok(true)` when both files report the same length. An unreadable
/// side is an error, not a difference — the audit must not claim drift
/// it could not observe.
///
/// Async because this runs once per file in the audit's hot loop; two
/// blocking `stat`s per file on the async runtime thread would freeze
/// every other IPC command on a slow share.
async fn compare_sizes(src: &Path, dst: &Path) -> Result<bool, String> {
    let (a, b) = tokio::join!(tokio::fs::metadata(src), tokio::fs::metadata(dst));
    let a = a.map_err(|e| format!("{}: {e}", src.display()))?;
    let b = b.map_err(|e| format!("{}: {e}", dst.display()))?;
    Ok(a.len() == b.len())
}

async fn record_history(state: &AppState, src: &Path, dst: &Path, report: &ReauditReport) {
    let Some(history) = state.history.clone() else {
        return;
    };
    let failed = report.differs + report.missing + report.extra + report.errors;
    if let Ok(row) = history
        .record_start(&JobSummary {
            kind: "verify".to_string(),
            status: "running".to_string(),
            src_root: src.to_path_buf(),
            dst_root: dst.to_path_buf(),
            verify_algo: Some(report.algo.clone()),
            ..Default::default()
        })
        .await
    {
        let _ = history
            .record_item(&ItemRow {
                job_row_id: row.0,
                src: src.to_path_buf(),
                dst: dst.to_path_buf(),
                size: 0,
                status: if report.clean() { "ok" } else { "failed" }.to_string(),
                hash_hex: None,
                error_code: None,
                error_msg: None,
                timestamp_ms: 0,
            })
            .await;
        let status = if report.clean() {
            "succeeded"
        } else {
            "failed"
        };
        let _ = history
            .record_finish(row, status, 0, report.ok, failed)
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walk_root_anchors_a_file_at_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("solo.bin");
        std::fs::write(&f, b"x").unwrap();
        let (anchor, rels) = walk_root(&f).unwrap();
        assert_eq!(anchor, dir.path());
        assert!(rels.contains(&PathBuf::from("solo.bin")));
    }

    #[test]
    fn walk_root_anchors_a_dir_at_itself() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("a.txt"), b"1").unwrap();
        let (anchor, rels) = walk_root(dir.path()).unwrap();
        assert_eq!(anchor, dir.path());
        assert_eq!(rels.len(), 1);
        assert!(rels.contains(&PathBuf::from("sub").join("a.txt")));
    }

    #[tokio::test]
    async fn compare_sizes_spots_a_length_difference() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"same").unwrap();
        std::fs::write(&b, b"same").unwrap();
        assert!(compare_sizes(&a, &b).await.unwrap());
        std::fs::write(&b, b"longer").unwrap();
        assert!(!compare_sizes(&a, &b).await.unwrap());
    }

    #[tokio::test]
    async fn compare_sizes_reports_a_missing_side_as_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        std::fs::write(&a, b"x").unwrap();
        assert!(compare_sizes(&a, &dir.path().join("ghost")).await.is_err());
    }

    #[test]
    fn report_is_clean_only_when_every_drift_counter_is_zero() {
        let mut report = ReauditReport {
            ok: 5,
            ..Default::default()
        };
        assert!(report.clean());
        report.extra = 1;
        assert!(!report.clean());
    }

    #[test]
    fn rows_are_bounded_but_counts_are_not() {
        let mut report = ReauditReport::default();
        for i in 0..(MAX_ROWS + 50) {
            report.missing += 1;
            report.push(Path::new(&format!("f{i}")), "missing", None);
        }
        assert_eq!(report.missing, (MAX_ROWS + 50) as u64);
        assert_eq!(report.rows.len(), MAX_ROWS);
    }

    #[test]
    fn relative_paths_render_with_forward_slashes() {
        let rel = Path::new("sub").join("deep").join("f.txt");
        assert_eq!(rel_display(&rel), "sub/deep/f.txt");
    }
}
