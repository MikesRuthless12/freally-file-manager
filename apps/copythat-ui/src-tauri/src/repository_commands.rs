//! Phase 49 — Tauri IPC for the unified content-addressed chunk
//! [`Repository`](copythat_chunk::Repository), surfaced by the Library
//! tab.
//!
//! Two read-only commands:
//!
//! - `repository_stats()` — the dedup hero numbers (bytes stored vs
//!   effective, distinct chunks, snapshot count, saved ratio).
//! - `repository_snapshots()` — the unified snapshot timeline
//!   (copy / sync / version / backup), oldest first.
//!
//! Both commands read the **shared** `Arc<Repository>` opened once at
//! startup and held in [`AppState`] (Phase 49b). Holding one handle is
//! what makes a process-lifetime open safe: redb takes an exclusive file
//! lock on `index.redb`, so the recovery web UI + mount features now share
//! this same handle instead of racing a second `open`. When the startup
//! open failed (no chunk store, or momentarily locked), `state.repository`
//! is `None` and the commands return the typed `"repository-unavailable"`
//! string the Library tab keys its empty state on. The reads are blocking,
//! so each command hops to a `spawn_blocking` worker rather than stalling
//! the async runtime.

use serde::Serialize;

use crate::state::AppState;

/// Wire shape for [`copythat_chunk::RepoStats`] plus the derived
/// `saved_ratio` (so the front end doesn't recompute it).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoStatsDto {
    /// Physical bytes on disk across pack files.
    pub stored_bytes: u64,
    /// Deduplicated logical size (sum of distinct chunk lengths).
    pub unique_bytes: u64,
    /// Sum of logical file sizes across every snapshot.
    pub effective_bytes: u64,
    /// Number of snapshots in the catalog.
    pub snapshot_count: u64,
    /// Number of distinct chunks indexed.
    pub chunk_count: u64,
    /// Fraction of effective bytes saved by dedup, in `0.0..=1.0`.
    pub saved_ratio: f64,
}

/// Wire shape for [`copythat_chunk::UnifiedSnapshot`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoSnapshotDto {
    /// Monotonic snapshot id.
    pub id: u64,
    /// `"copy"` / `"sync"` / `"version"` / `"backup"`.
    pub kind: String,
    /// Capture time, milliseconds since the Unix epoch.
    pub created_at_ms: i64,
    /// Human-readable label.
    pub label: String,
    /// Number of files captured.
    pub file_count: u64,
    /// Sum of logical file sizes (effective bytes this snapshot holds).
    pub total_size: u64,
}

/// `repository_stats()` — the dedup overview for the Library header
/// "hero" readout.
#[tauri::command]
pub async fn repository_stats(state: tauri::State<'_, AppState>) -> Result<RepoStatsDto, String> {
    let repo = state.repository.clone().ok_or("repository-unavailable")?;
    let stats = tokio::task::spawn_blocking(move || repo.stats())
        .await
        .map_err(|e| format!("repository stats task: {e}"))?
        .map_err(|e| format!("repository stats: {e}"))?;
    Ok(RepoStatsDto {
        stored_bytes: stats.stored_bytes,
        unique_bytes: stats.unique_bytes,
        effective_bytes: stats.effective_bytes,
        snapshot_count: stats.snapshot_count,
        chunk_count: stats.chunk_count,
        saved_ratio: stats.saved_ratio(),
    })
}

/// `repository_snapshots()` — the unified snapshot timeline, oldest
/// first. Reads the lightweight summaries index (no chunk manifests).
#[tauri::command]
pub async fn repository_snapshots(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<RepoSnapshotDto>, String> {
    let repo = state.repository.clone().ok_or("repository-unavailable")?;
    let snaps = tokio::task::spawn_blocking(move || repo.snapshots())
        .await
        .map_err(|e| format!("repository snapshots task: {e}"))?
        .map_err(|e| format!("repository snapshots: {e}"))?;
    Ok(snaps
        .into_iter()
        .map(|s| RepoSnapshotDto {
            id: s.id,
            kind: s.kind.as_str().to_string(),
            created_at_ms: s.created_at_ms,
            label: s.label,
            file_count: s.file_count,
            total_size: s.total_size,
        })
        .collect())
}

/// Phase 49b — the previously-deferred Phase 42 snapshot-on-overwrite
/// hook, realised against the unified [`Repository`](copythat_chunk::Repository).
///
/// The engine calls
/// [`snapshot_before_overwrite`](copythat_core::VersioningSink::snapshot_before_overwrite)
/// once per file — AFTER the collision policy resolves to "overwrite" but
/// BEFORE the destination is truncated — and already wraps the call in
/// `spawn_blocking`, so this impl does its (blocking) chunk-store work
/// directly. We capture the about-to-be-clobbered destination as a
/// `Version` snapshot so the Library timeline can roll a file back to a
/// prior state. Best-effort: a failure returns `Err` and the engine
/// proceeds with the copy (the snapshot is lost; the data is not).
#[derive(Debug)]
pub struct RepositoryVersioningSink {
    repo: std::sync::Arc<copythat_chunk::Repository>,
}

impl RepositoryVersioningSink {
    /// Wrap the shared repository handle.
    #[must_use]
    pub fn new(repo: std::sync::Arc<copythat_chunk::Repository>) -> Self {
        Self { repo }
    }
}

impl copythat_core::VersioningSink for RepositoryVersioningSink {
    fn snapshot_before_overwrite(
        &self,
        dst: &std::path::Path,
        _triggered_by_job_id: Option<i64>,
    ) -> Result<bool, String> {
        // The engine already confirmed dst exists; re-check so a direct
        // caller can't trip a spurious read.
        if !dst.exists() {
            return Ok(false);
        }
        let label = dst.to_string_lossy().into_owned();
        let now = chrono::Utc::now().timestamp_millis();
        self.repo
            .snapshot_files(
                copythat_chunk::SnapshotKind::Version,
                &label,
                now,
                &[(label.as_str(), dst)],
            )
            .map(|_| true)
            .map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------
// Phase 49d — restore browser.
// ---------------------------------------------------------------------

/// One file in a snapshot's flat tree (path + logical size).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotFileDto {
    pub path: String,
    pub size: u64,
}

/// Per-file existence check that drives the restore conflict step.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreConflictDto {
    pub path: String,
    pub exists: bool,
}

/// Tally of a restore run.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreReportDto {
    pub restored: u64,
    pub skipped: u64,
    pub failed: u64,
}

/// Wire shape for [`copythat_chunk::GcReport`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GcReportDto {
    pub chunks_swept: u64,
    pub packs_removed: u64,
    pub bytes_reclaimed: u64,
}

/// Map the wire conflict string to the chunk-layer policy. Only the three
/// explicit choices act; anything unrecognised (a front-end typo, an
/// outdated webview) falls back to the SAFE non-destructive `Skip` rather
/// than silently overwriting the user's existing files.
fn parse_conflict(s: &str) -> copythat_chunk::RestoreConflict {
    match s {
        "overwrite" => copythat_chunk::RestoreConflict::Overwrite,
        "keep-both" => copythat_chunk::RestoreConflict::KeepBoth,
        _ => copythat_chunk::RestoreConflict::Skip,
    }
}

/// The strip-prefix that relativises a snapshot's logical paths for restore.
/// Copy / sync / version snapshots key files by their ABSOLUTE path;
/// restoring those verbatim would recreate the whole original tree under the
/// chosen destination. Stripping the deepest common directory of the
/// selected ABSOLUTE keys instead restores structure relative to the
/// copy/source root (a single file flattens to its name). Backup snapshots
/// already use source-relative keys → `None` (used as-is).
fn restore_strip_prefix(paths: &[String]) -> Option<String> {
    let first = paths.first()?;
    let win_abs = |s: &str| {
        let b = s.as_bytes();
        b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
    };
    if !(first.starts_with('/') || first.starts_with('\\') || win_abs(first)) {
        return None; // relative keys (backup) — keep their subtree as-is
    }
    let norm: Vec<String> = paths.iter().map(|p| p.replace('\\', "/")).collect();
    // Common prefix over directory components (exclude each path's file name).
    let mut common: Vec<&str> = norm[0].split('/').collect();
    common.pop();
    for p in &norm[1..] {
        let parts: Vec<&str> = p.split('/').collect();
        let upto = parts.len().saturating_sub(1);
        let mut i = 0;
        while i < common.len() && i < upto && common[i] == parts[i] {
            i += 1;
        }
        common.truncate(i);
        if common.is_empty() {
            break;
        }
    }
    if common.is_empty() {
        None
    } else {
        Some(common.join("/"))
    }
}

/// `snapshot_tree` — flat file listing of a snapshot for the restore tree.
#[tauri::command]
pub async fn snapshot_tree(
    state: tauri::State<'_, AppState>,
    snapshot_id: u64,
) -> Result<Vec<SnapshotFileDto>, String> {
    let repo = state.repository.clone().ok_or("repository-unavailable")?;
    let entries = tokio::task::spawn_blocking(move || {
        repo.snapshot_tree(copythat_chunk::SnapshotId(snapshot_id))
    })
    .await
    .map_err(|e| format!("snapshot tree task: {e}"))?
    .map_err(|e| format!("snapshot tree: {e}"))?;
    Ok(entries
        .into_iter()
        .map(|e| SnapshotFileDto {
            path: e.path,
            size: e.size,
        })
        .collect())
}

/// `restore_preview` — which selected files already exist at the target.
#[tauri::command]
pub async fn restore_preview(
    state: tauri::State<'_, AppState>,
    snapshot_id: u64,
    paths: Vec<String>,
    dst_root: String,
) -> Result<Vec<RestoreConflictDto>, String> {
    let repo = state.repository.clone().ok_or("repository-unavailable")?;
    let dst = std::path::PathBuf::from(dst_root);
    let strip = restore_strip_prefix(&paths);
    let pairs = tokio::task::spawn_blocking(move || {
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        repo.restore_preview(
            copythat_chunk::SnapshotId(snapshot_id),
            &refs,
            &dst,
            strip.as_deref(),
        )
    })
    .await
    .map_err(|e| format!("restore preview task: {e}"))?
    .map_err(|e| format!("restore preview: {e}"))?;
    Ok(pairs
        .into_iter()
        .map(|(path, exists)| RestoreConflictDto { path, exists })
        .collect())
}

/// `restore_paths` — restore the selected files under `dst_root`.
#[tauri::command]
pub async fn restore_paths(
    state: tauri::State<'_, AppState>,
    snapshot_id: u64,
    paths: Vec<String>,
    dst_root: String,
    conflict: String,
) -> Result<RestoreReportDto, String> {
    let repo = state.repository.clone().ok_or("repository-unavailable")?;
    let dst = std::path::PathBuf::from(dst_root);
    let policy = parse_conflict(&conflict);
    let strip = restore_strip_prefix(&paths);
    let report = tokio::task::spawn_blocking(move || {
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        repo.restore_paths(
            copythat_chunk::SnapshotId(snapshot_id),
            &refs,
            &dst,
            strip.as_deref(),
            policy,
        )
    })
    .await
    .map_err(|e| format!("restore task: {e}"))?
    .map_err(|e| format!("restore: {e}"))?;
    Ok(RestoreReportDto {
        restored: report.restored,
        skipped: report.skipped,
        failed: report.failed,
    })
}

/// `repository_forget` — drop a snapshot from the catalog. Chunk bytes are
/// reclaimed by a later `repository_gc`, not here.
#[tauri::command]
pub async fn repository_forget(
    state: tauri::State<'_, AppState>,
    snapshot_id: u64,
) -> Result<bool, String> {
    let repo = state.repository.clone().ok_or("repository-unavailable")?;
    tokio::task::spawn_blocking(move || {
        repo.remove_snapshot(copythat_chunk::SnapshotId(snapshot_id))
    })
    .await
    .map_err(|e| format!("forget task: {e}"))?
    .map_err(|e| format!("forget: {e}"))
}

/// `repository_gc` — mark-and-sweep unreferenced chunks; reclaim space.
#[tauri::command]
pub async fn repository_gc(state: tauri::State<'_, AppState>) -> Result<GcReportDto, String> {
    let repo = state.repository.clone().ok_or("repository-unavailable")?;
    let report = tokio::task::spawn_blocking(move || repo.gc())
        .await
        .map_err(|e| format!("gc task: {e}"))?
        .map_err(|e| format!("gc: {e}"))?;
    Ok(GcReportDto {
        chunks_swept: report.chunks_swept,
        packs_removed: report.packs_removed,
        bytes_reclaimed: report.bytes_reclaimed,
    })
}
