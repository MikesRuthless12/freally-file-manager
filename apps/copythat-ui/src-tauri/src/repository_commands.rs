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
//! Both return a typed `"repository-unavailable"` error string when the
//! repository failed to open at startup (the Library tab renders its
//! empty/unavailable state). The repository's `redb` reads are blocking,
//! so each command hops to a `spawn_blocking` worker rather than stalling
//! the async runtime — the same discipline the history commands use.

use std::sync::Arc;

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

/// Clone the shared `Repository` handle, or surface the typed
/// unavailable error the Library tab keys its empty state on.
fn require_repository(
    state: &tauri::State<'_, AppState>,
) -> Result<Arc<copythat_chunk::Repository>, String> {
    state
        .inner()
        .repository
        .clone()
        .ok_or_else(|| "repository-unavailable".to_string())
}

/// `repository_stats()` — the dedup overview for the Library header
/// "hero" readout.
#[tauri::command]
pub async fn repository_stats(state: tauri::State<'_, AppState>) -> Result<RepoStatsDto, String> {
    let repo = require_repository(&state)?;
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
    let repo = require_repository(&state)?;
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
