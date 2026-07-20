//! FFM-M02 — transactional undo of the last copy / move operation.
//!
//! The Phase 9 history DB already records every completed item
//! (src / dst / size / verify hash), so undo is a *planned reversal*
//! over those rows rather than a blind delete:
//!
//! - **Copy job** → remove each verified destination — only when the
//!   file on disk still matches what the job wrote (size, plus the
//!   recorded verify hash when one exists). Removals go to the OS
//!   trash (never a permanent unlink), so even a mistaken undo is
//!   itself recoverable.
//! - **Move job** → move each destination back to its original source
//!   path (rename first, cross-device copy+delete fallback); a row
//!   whose original path is now occupied is a conflict and is skipped.
//!
//! `undo_plan` returns a full preview (per-row action + status) that
//! the UndoPreviewModal shows before anything is touched; `undo_apply`
//! re-checks each row immediately before acting and records the undo
//! as its own history job (`undo-copy` / `undo-move`), which is what
//! makes the history multi-level: every undo is itself visible,
//! auditable, and (for moves) reversible.

use std::path::Path;
use std::str::FromStr;

use serde::Serialize;
use tauri::State;

use freally_history::{HistoryFilter, ItemRow, JobRowId, JobSummary};

use crate::state::AppState;

/// Row statuses. `ready` rows are the only ones `undo_apply` touches.
pub const STATUS_READY: &str = "ready";
pub const STATUS_SKIP_MISSING: &str = "skip-missing";
pub const STATUS_SKIP_CHANGED: &str = "skip-changed";
pub const STATUS_CONFLICT: &str = "conflict";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoRowDto {
    pub src: String,
    pub dst: String,
    pub size: u64,
    /// `"trash-dst"` (copy undo) or `"move-back"` (move undo).
    pub action: String,
    /// One of the `STATUS_*` strings above.
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoPlanDto {
    pub job_id: i64,
    /// The original job's kind: `"copy"` or `"move"`.
    pub kind: String,
    pub rows: Vec<UndoRowDto>,
    pub ready: u64,
    pub skipped: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoReportDto {
    pub done: u64,
    pub skipped: u64,
    pub failed: u64,
}

/// Classify one copy-job item for undo, given what's on disk now.
/// Pure — probes are passed in so the decision table is unit-tested
/// without touching the filesystem.
fn classify_copy_undo(
    dst_len: Option<u64>,
    recorded_size: u64,
    hash_matches: Option<bool>,
) -> &'static str {
    match dst_len {
        None => STATUS_SKIP_MISSING,
        Some(len) if len != recorded_size => STATUS_SKIP_CHANGED,
        Some(_) => match hash_matches {
            Some(false) => STATUS_SKIP_CHANGED,
            // No recorded hash → size match is the best we have.
            Some(true) | None => STATUS_READY,
        },
    }
}

/// Classify one move-job item for undo (move `dst` back to `src`).
fn classify_move_undo(dst_exists: bool, src_exists: bool) -> &'static str {
    if !dst_exists {
        STATUS_SKIP_MISSING
    } else if src_exists {
        STATUS_CONFLICT
    } else {
        STATUS_READY
    }
}

/// Re-hash `path` with the job's verify algorithm and compare against
/// the recorded hex digest. `None` when the job recorded no hash (or
/// the algorithm string no longer parses).
async fn hash_matches_recorded(
    path: &Path,
    algo: Option<freally_hash::HashAlgorithm>,
    recorded_hex: Option<&str>,
) -> Result<Option<bool>, String> {
    let (Some(algo), Some(recorded)) = (algo, recorded_hex) else {
        return Ok(None);
    };
    let hex = crate::hashing::hash_file_hex(path, algo).await?;
    Ok(Some(hex.eq_ignore_ascii_case(recorded)))
}

fn file_len(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()
        .filter(|m| m.is_file())
        .map(|m| m.len())
}

/// Build the preview plan for undoing `job_id`. Read-only.
async fn build_plan(state: &AppState, job_id: i64) -> Result<UndoPlanDto, String> {
    let history = state.history.clone().ok_or("history is disabled")?;
    let job = history
        .get(JobRowId(job_id))
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("history job {job_id} not found"))?;
    if job.kind != "copy" && job.kind != "move" {
        return Err(format!("undo is not supported for `{}` jobs", job.kind));
    }
    let algo = job
        .verify_algo
        .as_deref()
        .and_then(|s| freally_hash::HashAlgorithm::from_str(s).ok());
    let items = history
        .items_for(JobRowId(job_id))
        .await
        .map_err(|e| e.to_string())?;

    let mut rows = Vec::new();
    for item in items.iter().filter(|i| i.status == "ok") {
        rows.push(plan_row(&job.kind, item, algo).await?);
    }
    let ready = rows.iter().filter(|r| r.status == STATUS_READY).count() as u64;
    let skipped = rows.len() as u64 - ready;
    Ok(UndoPlanDto {
        job_id,
        kind: job.kind,
        rows,
        ready,
        skipped,
    })
}

async fn plan_row(
    kind: &str,
    item: &ItemRow,
    algo: Option<freally_hash::HashAlgorithm>,
) -> Result<UndoRowDto, String> {
    let (action, status) = if kind == "copy" {
        let dst_len = file_len(&item.dst);
        // Only pay the re-hash when the cheap size gate passed.
        let hash_ok = if dst_len == Some(item.size) {
            hash_matches_recorded(&item.dst, algo, item.hash_hex.as_deref()).await?
        } else {
            None
        };
        ("trash-dst", classify_copy_undo(dst_len, item.size, hash_ok))
    } else {
        (
            "move-back",
            classify_move_undo(item.dst.exists(), item.src.exists()),
        )
    };
    Ok(UndoRowDto {
        src: item.src.to_string_lossy().into_owned(),
        dst: item.dst.to_string_lossy().into_owned(),
        size: item.size,
        action: action.to_string(),
        status: status.to_string(),
    })
}

/// Preview what undoing `job_id` would touch. Never writes.
#[tauri::command]
pub async fn undo_plan(job_id: i64, state: State<'_, AppState>) -> Result<UndoPlanDto, String> {
    build_plan(state.inner(), job_id).await
}

/// The newest succeeded copy/move job — the Ctrl+Z target — with its
/// plan, or `None` when history holds nothing undoable.
#[tauri::command]
pub async fn undo_last_candidate(
    state: State<'_, AppState>,
) -> Result<Option<UndoPlanDto>, String> {
    let history = state.history.clone().ok_or("history is disabled")?;
    let mut newest: Option<JobSummary> = None;
    for kind in ["copy", "move"] {
        let filter = HistoryFilter {
            kind: Some(kind.to_string()),
            status: Some("succeeded".to_string()),
            limit: Some(1),
            ..Default::default()
        };
        if let Some(job) = history
            .search(filter)
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .next()
        {
            let newer = newest
                .as_ref()
                .is_none_or(|n| job.started_at_ms > n.started_at_ms);
            if newer {
                newest = Some(job);
            }
        }
    }
    match newest {
        Some(job) => Ok(Some(build_plan(state.inner(), job.row_id).await?)),
        None => Ok(None),
    }
}

/// Execute the undo: re-check each row immediately before acting, then
/// trash copied destinations / move moved files back. Records the undo
/// as its own history job so it is auditable and visible in the
/// drawer.
#[tauri::command]
pub async fn undo_apply(job_id: i64, state: State<'_, AppState>) -> Result<UndoReportDto, String> {
    let plan = build_plan(state.inner(), job_id).await?;
    let history = state.history.clone().ok_or("history is disabled")?;

    // The undo itself is a history job: src/dst roots swapped relative
    // to the original so the drawer reads naturally ("undo-copy from
    // <dst_root>").
    let original = history
        .get(JobRowId(job_id))
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("history job {job_id} not found"))?;
    let undo_kind = format!("undo-{}", plan.kind);
    let undo_row = history
        .record_start(&JobSummary {
            kind: undo_kind,
            status: "running".to_string(),
            // src/dst roots swapped vs the original so the drawer reads
            // "undo-copy from <dst_root>". record_start stamps the clock.
            src_root: original.dst_root.clone(),
            dst_root: original.src_root.clone(),
            ..Default::default()
        })
        .await
        .map_err(|e| e.to_string())?;

    let mut report = UndoReportDto::default();
    let mut bytes: u64 = 0;
    for row in &plan.rows {
        if row.status != STATUS_READY {
            report.skipped += 1;
            continue;
        }
        let outcome = apply_row(&plan.kind, row).await;
        let (status, error_msg) = match &outcome {
            Ok(()) => {
                report.done += 1;
                bytes += row.size;
                ("ok", None)
            }
            Err(e) => {
                report.failed += 1;
                ("failed", Some(e.clone()))
            }
        };
        // Item rows describe the undo's own direction (what moved
        // where): trash-dst rows have no meaningful destination.
        let _ = history
            .record_item(&ItemRow {
                job_row_id: undo_row.0,
                src: row.dst.clone().into(),
                dst: row.src.clone().into(),
                size: row.size,
                status: status.to_string(),
                hash_hex: None,
                error_code: None,
                error_msg,
                timestamp_ms: 0, // record_item stamps the clock
            })
            .await;
    }

    let final_status = if report.failed == 0 {
        "succeeded"
    } else {
        "failed"
    };
    let _ = history
        .record_finish(undo_row, final_status, bytes, report.done, report.failed)
        .await;
    Ok(report)
}

/// Execute one ready row, re-verifying on the spot so a file that
/// changed between preview and apply is left alone.
async fn apply_row(kind: &str, row: &UndoRowDto) -> Result<(), String> {
    let src = std::path::PathBuf::from(&row.src);
    let dst = std::path::PathBuf::from(&row.dst);
    if kind == "copy" {
        // Cheap re-check: size only (the hash was verified seconds ago
        // in the plan; a size flip is the tamper signal we can afford
        // twice). Then park in the OS trash — never a hard unlink.
        if file_len(&dst) != Some(row.size) {
            return Err(format!("changed since preview: {}", row.dst));
        }
        tauri::async_runtime::spawn_blocking(move || trash::delete(&dst).map_err(|e| e.to_string()))
            .await
            .map_err(|e| e.to_string())?
    } else {
        if !dst.exists() {
            return Err(format!("missing: {}", row.dst));
        }
        if src.exists() {
            return Err(format!("original path now occupied: {}", row.src));
        }
        if let Some(parent) = src.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        match std::fs::rename(&dst, &src) {
            Ok(()) => Ok(()),
            // Cross-device: plain copy back + remove, same helpers the
            // paste chooser's System row uses.
            Err(_) => {
                let mut scratch = crate::shell_commands::SystemPasteReport::default();
                if let Err(e) = crate::shell_commands::plain_transfer(&dst, &src, &mut scratch) {
                    // The copy-back failed partway: `dst` (the authoritative
                    // data) is untouched, but a partial may now sit at `src`
                    // and would make every later undo report "original path
                    // occupied". Roll the partial back so the undo stays
                    // retryable.
                    let _ = crate::shell_commands::remove_source(&src);
                    return Err(e);
                }
                crate::shell_commands::remove_source(&dst)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_undo_classification_table() {
        // Missing destination.
        assert_eq!(classify_copy_undo(None, 10, None), STATUS_SKIP_MISSING);
        // Size drifted.
        assert_eq!(classify_copy_undo(Some(11), 10, None), STATUS_SKIP_CHANGED);
        // Size matches, no hash recorded → best effort ready.
        assert_eq!(classify_copy_undo(Some(10), 10, None), STATUS_READY);
        // Hash mismatch beats a size match.
        assert_eq!(
            classify_copy_undo(Some(10), 10, Some(false)),
            STATUS_SKIP_CHANGED
        );
        // Hash confirmed.
        assert_eq!(classify_copy_undo(Some(10), 10, Some(true)), STATUS_READY);
    }

    #[test]
    fn move_undo_classification_table() {
        assert_eq!(classify_move_undo(false, false), STATUS_SKIP_MISSING);
        assert_eq!(classify_move_undo(true, true), STATUS_CONFLICT);
        assert_eq!(classify_move_undo(true, false), STATUS_READY);
    }

    #[tokio::test]
    async fn apply_row_moves_back_and_respects_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("orig").join("a.txt");
        let moved = dir.path().join("moved").join("a.txt");
        std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
        std::fs::write(&moved, b"payload").unwrap();

        let row = UndoRowDto {
            src: original.to_string_lossy().into_owned(),
            dst: moved.to_string_lossy().into_owned(),
            size: 7,
            action: "move-back".to_string(),
            status: STATUS_READY.to_string(),
        };
        apply_row("move", &row).await.unwrap();
        assert!(original.is_file());
        assert!(!moved.exists());

        // Second application: dst is gone → clean error, no clobber.
        let err = apply_row("move", &row).await.unwrap_err();
        assert!(err.contains("missing"), "{err}");

        // Conflict: both sides exist → refused.
        std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
        std::fs::write(&moved, b"payload").unwrap();
        let err = apply_row("move", &row).await.unwrap_err();
        assert!(err.contains("occupied"), "{err}");
    }

    #[tokio::test]
    async fn apply_row_copy_refuses_drifted_destination() {
        let dir = tempfile::tempdir().unwrap();
        let dst = dir.path().join("copied.bin");
        std::fs::write(&dst, b"123456").unwrap();
        let row = UndoRowDto {
            src: dir.path().join("src.bin").to_string_lossy().into_owned(),
            dst: dst.to_string_lossy().into_owned(),
            size: 999, // recorded size disagrees with disk
            action: "trash-dst".to_string(),
            status: STATUS_READY.to_string(),
        };
        let err = apply_row("copy", &row).await.unwrap_err();
        assert!(err.contains("changed since preview"), "{err}");
        assert!(dst.exists(), "drifted file must be left alone");
    }
}
