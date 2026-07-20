//! FFM-M03 — trash-aware delete.
//!
//! A plain "Delete to Recycle Bin / Trash" action (today the app only
//! offered irreversible shred). Files go to the OS trash via the
//! `trash` crate — recoverable by design — and each batch is recorded
//! as a `delete` history job so the drawer shows what was removed.
//!
//! The two *safety-net* halves of FFM-M03 (park an overwritten file in
//! the trash, send a moved source to the trash) are honored by the
//! copy/move enqueue path via [`trash_paths`]; this module owns the
//! standalone delete command and the shared trash helper.

use serde::Serialize;
use tauri::State;

use freally_history::{ItemRow, JobSummary};

use crate::ipc_safety::validate_ipc_path;
use crate::state::AppState;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashReport {
    pub trashed: u64,
    pub failed: u64,
}

/// Send `paths` to the OS trash on a blocking thread. Shared by the
/// delete command and the copy/move safety nets. Returns the per-path
/// results paired with the input so callers can record history.
pub(crate) async fn trash_paths(
    paths: Vec<std::path::PathBuf>,
) -> Vec<(std::path::PathBuf, Result<(), String>)> {
    tauri::async_runtime::spawn_blocking(move || {
        paths
            .into_iter()
            .map(|p| {
                let r = trash::delete(&p).map_err(|e| e.to_string());
                (p, r)
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// Delete a selection to the OS trash and record it in history.
/// Recoverable — this is the safe counterpart to secure-delete.
#[tauri::command]
pub async fn trash_delete(
    paths: Vec<String>,
    state: State<'_, AppState>,
) -> Result<TrashReport, String> {
    // IPC-gate every path (same U+FFFD / traversal guard the copy
    // commands use) before touching the filesystem.
    let mut validated = Vec::with_capacity(paths.len());
    for p in &paths {
        validated.push(validate_ipc_path(p).map_err(|e| e.to_string())?);
    }
    if validated.is_empty() {
        return Ok(TrashReport::default());
    }

    let results = trash_paths(validated).await;

    // Record the whole batch as one `delete` history job.
    let mut report = TrashReport::default();
    if let Some(history) = state.history.clone() {
        let common = common_parent(results.iter().map(|(p, _)| p.as_path()));
        if let Ok(row) = history
            .record_start(&JobSummary {
                kind: "delete".to_string(),
                status: "running".to_string(),
                src_root: common.clone(),
                dst_root: std::path::PathBuf::from("(trash)"),
                ..Default::default()
            })
            .await
        {
            for (path, outcome) in &results {
                let (status, error_msg) = match outcome {
                    Ok(()) => {
                        report.trashed += 1;
                        ("ok", None)
                    }
                    Err(e) => {
                        report.failed += 1;
                        ("failed", Some(e.clone()))
                    }
                };
                let _ = history
                    .record_item(&ItemRow {
                        job_row_id: row.0,
                        src: path.clone(),
                        dst: std::path::PathBuf::from("(trash)"),
                        size: 0,
                        status: status.to_string(),
                        hash_hex: None,
                        error_code: None,
                        error_msg,
                        timestamp_ms: 0,
                    })
                    .await;
            }
            let final_status = if report.failed == 0 {
                "succeeded"
            } else {
                "failed"
            };
            let _ = history
                .record_finish(row, final_status, 0, report.trashed, report.failed)
                .await;
        }
    } else {
        for (_, outcome) in &results {
            match outcome {
                Ok(()) => report.trashed += 1,
                Err(_) => report.failed += 1,
            }
        }
    }
    Ok(report)
}

/// Longest shared parent of a set of paths (for the history job's
/// `src_root`), or the first path's parent as a fallback.
fn common_parent<'a>(paths: impl Iterator<Item = &'a std::path::Path>) -> std::path::PathBuf {
    let mut prefix: Option<std::path::PathBuf> = None;
    for p in paths {
        let parent = p.parent().unwrap_or(p).to_path_buf();
        prefix = Some(match prefix {
            None => parent,
            Some(cur) => shared_prefix(&cur, &parent),
        });
    }
    prefix.unwrap_or_default()
}

fn shared_prefix(a: &std::path::Path, b: &std::path::Path) -> std::path::PathBuf {
    let mut out = std::path::PathBuf::new();
    for (ca, cb) in a.components().zip(b.components()) {
        if ca == cb {
            out.push(ca);
        } else {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn shared_prefix_finds_common_root() {
        let a = Path::new("/a/b/c/file1");
        let b = Path::new("/a/b/d/file2");
        assert_eq!(shared_prefix(a, b), Path::new("/a/b"));
    }

    #[test]
    fn common_parent_of_siblings_is_their_folder() {
        let paths = [Path::new("/data/x.txt"), Path::new("/data/y.txt")];
        assert_eq!(common_parent(paths.iter().copied()), Path::new("/data"));
    }

    #[tokio::test]
    async fn trash_paths_reports_per_path_results() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        std::fs::write(&real, b"bye").unwrap();
        let missing = dir.path().join("nope.txt");

        // One result per input, in order, and no panic. Whether the OS
        // actually trashes the real file is environment-dependent (a
        // headless CI runner may lack a Trash/Recycle Bin), so we don't
        // assert its success — only that a nonexistent path is a clean
        // per-path error rather than a crash.
        let results = trash_paths(vec![real.clone(), missing.clone()]).await;
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, real);
        assert_eq!(results[1].0, missing);
        assert!(results[1].1.is_err());
    }
}
