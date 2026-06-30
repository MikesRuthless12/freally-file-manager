//! Phase 49c — Tauri IPC for backup sources.
//!
//! A *source* is a folder the user designates for backup; "Back up now"
//! enumerates it (honouring exclude globs) and snapshots it into the
//! shared unified [`Repository`](copythat_chunk::Repository) as a
//! [`SnapshotKind::Backup`](copythat_chunk::SnapshotKind::Backup). CRUD
//! mirrors `sync_commands` — persisted under `Settings::backup.sources`,
//! UUID-keyed so a label edit never loses the timeline association.
//! Because an unchanged tree's chunks all dedup, a repeat backup adds
//! ~zero stored bytes, so "Back up now" is cheap to run often.

use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

use crate::ipc::{
    EVENT_BACKUP_COMPLETED, EVENT_BACKUP_FAILED, EVENT_BACKUP_PROGRESS, EVENT_BACKUP_STARTED,
};
use crate::state::AppState;

/// Wire shape for [`copythat_settings::SourceConfig`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceConfigDto {
    pub id: String,
    pub label: String,
    pub path: String,
    pub exclude_globs: Vec<String>,
    pub last_run_at: String,
    pub last_snapshot_id: Option<u64>,
}

impl From<&copythat_settings::SourceConfig> for SourceConfigDto {
    fn from(c: &copythat_settings::SourceConfig) -> Self {
        Self {
            id: c.id.clone(),
            label: c.label.clone(),
            path: c.path.clone(),
            exclude_globs: c.exclude_globs.clone(),
            last_run_at: c.last_run_at.clone(),
            last_snapshot_id: c.last_snapshot_id,
        }
    }
}

// --- event payloads ---------------------------------------------------

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupStartedDto {
    id: String,
    label: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupProgressDto {
    id: String,
    files_done: u64,
    files_total: u64,
    bytes_done: u64,
    bytes_total: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupCompletedDto {
    id: String,
    snapshot_id: u64,
    files: u64,
    bytes: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupFailedDto {
    id: String,
    message: String,
}

// --- CRUD -------------------------------------------------------------

/// List every configured backup source.
#[tauri::command]
pub fn sources_list(state: tauri::State<'_, AppState>) -> Vec<SourceConfigDto> {
    state
        .settings_snapshot()
        .backup
        .sources
        .iter()
        .map(SourceConfigDto::from)
        .collect()
}

/// Add a new backup source and persist settings.
#[tauri::command]
pub fn sources_add(
    label: String,
    path: String,
    exclude_globs: Vec<String>,
    state: tauri::State<'_, AppState>,
) -> Result<SourceConfigDto, String> {
    if path.trim().is_empty() {
        return Err("backup source path is required".to_string());
    }
    let cfg = copythat_settings::SourceConfig {
        id: Uuid::new_v4().hyphenated().to_string(),
        label,
        path,
        exclude_globs,
        last_run_at: String::new(),
        last_snapshot_id: None,
    };
    {
        let mut guard = state.settings.write().map_err(|e| e.to_string())?;
        guard.backup.sources.push(cfg.clone());
        persist(&state, &guard)?;
    }
    Ok(SourceConfigDto::from(&cfg))
}

/// Edit an existing source's label / path / excludes.
#[tauri::command]
pub fn sources_update(
    id: String,
    label: String,
    path: String,
    exclude_globs: Vec<String>,
    state: tauri::State<'_, AppState>,
) -> Result<SourceConfigDto, String> {
    let mut guard = state.settings.write().map_err(|e| e.to_string())?;
    let dto = {
        let src = guard
            .backup
            .sources
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| format!("unknown source id: {id}"))?;
        src.label = label;
        src.path = path;
        src.exclude_globs = exclude_globs;
        SourceConfigDto::from(&*src)
    };
    persist(&state, &guard)?;
    Ok(dto)
}

/// Remove a source by id.
#[tauri::command]
pub fn sources_remove(id: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let mut guard = state.settings.write().map_err(|e| e.to_string())?;
    let before = guard.backup.sources.len();
    guard.backup.sources.retain(|s| s.id != id);
    if guard.backup.sources.len() == before {
        return Err(format!("unknown source id: {id}"));
    }
    persist(&state, &guard)
}

/// "Back up now": enumerate the source tree (honouring exclude globs),
/// snapshot it into the shared Repository as `SnapshotKind::Backup`,
/// persist `last_run_at` + `last_snapshot_id`, and emit `backup-*`
/// events. Returns the new snapshot id.
#[tauri::command]
pub async fn backup_now(
    app: AppHandle,
    id: String,
    state: tauri::State<'_, AppState>,
) -> Result<u64, String> {
    let repo = state.repository.clone().ok_or("repository-unavailable")?;
    let cfg = state
        .settings_snapshot()
        .backup
        .sources
        .iter()
        .find(|s| s.id == id)
        .cloned()
        .ok_or_else(|| format!("unknown source id: {id}"))?;

    let _ = app.emit(
        EVENT_BACKUP_STARTED,
        BackupStartedDto {
            id: id.clone(),
            label: cfg.label.clone(),
        },
    );

    let app_for_blocking = app.clone();
    let id_for_blocking = id.clone();
    let root = PathBuf::from(&cfg.path);
    let label = cfg.label.clone();
    let excludes = cfg.exclude_globs.clone();

    let outcome = tokio::task::spawn_blocking(move || -> Result<(u64, u64, u64), String> {
        let globs = build_globset(&excludes)?;
        let files = enumerate_source(&root, &globs);
        let files_total = files.len() as u64;
        let bytes_total: u64 = files.iter().map(|(_, _, size)| *size).sum();
        // Single streaming snapshot pass: emit 0% up front, 100% after.
        // (A per-file progress would need a chunk-store callback the
        // `snapshot_files` API doesn't expose yet.)
        let _ = app_for_blocking.emit(
            EVENT_BACKUP_PROGRESS,
            BackupProgressDto {
                id: id_for_blocking.clone(),
                files_done: 0,
                files_total,
                bytes_done: 0,
                bytes_total,
            },
        );
        let pairs: Vec<(&str, &Path)> = files
            .iter()
            .map(|(key, abs, _)| (key.as_str(), abs.as_path()))
            .collect();
        let now = chrono::Utc::now().timestamp_millis();
        let snap = repo
            .snapshot_files(copythat_chunk::SnapshotKind::Backup, &label, now, &pairs)
            .map_err(|e| e.to_string())?;
        let _ = app_for_blocking.emit(
            EVENT_BACKUP_PROGRESS,
            BackupProgressDto {
                id: id_for_blocking,
                files_done: files_total,
                files_total,
                bytes_done: bytes_total,
                bytes_total,
            },
        );
        Ok((snap.as_u64(), files_total, bytes_total))
    })
    .await
    .map_err(|e| format!("backup task: {e}"))?;

    match outcome {
        Ok((snapshot_id, files, bytes)) => {
            {
                let mut guard = state.settings.write().map_err(|e| e.to_string())?;
                if let Some(src) = guard.backup.sources.iter_mut().find(|s| s.id == id) {
                    src.last_run_at = chrono::Utc::now().to_rfc3339();
                    src.last_snapshot_id = Some(snapshot_id);
                }
                persist(&state, &guard)?;
            }
            let _ = app.emit(
                EVENT_BACKUP_COMPLETED,
                BackupCompletedDto {
                    id,
                    snapshot_id,
                    files,
                    bytes,
                },
            );
            Ok(snapshot_id)
        }
        Err(message) => {
            let _ = app.emit(
                EVENT_BACKUP_FAILED,
                BackupFailedDto {
                    id,
                    message: message.clone(),
                },
            );
            Err(message)
        }
    }
}

// --- helpers ----------------------------------------------------------

fn persist(state: &AppState, settings: &copythat_settings::Settings) -> Result<(), String> {
    let path = state.settings_path.as_path();
    if path.as_os_str().is_empty() {
        return Ok(()); // tests use an empty path; skip persistence
    }
    settings
        .save_to(path)
        .map_err(|e| format!("save settings: {e}"))
}

/// Build a `GlobSet` from the user's exclude patterns. An empty list →
/// an empty set that matches nothing. A malformed glob is a hard error so
/// the user sees their typo rather than a silent "excluded nothing".
fn build_globset(patterns: &[String]) -> Result<GlobSet, String> {
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        let g = Glob::new(p).map_err(|e| format!("invalid exclude glob `{p}`: {e}"))?;
        builder.add(g);
    }
    builder.build().map_err(|e| format!("build glob set: {e}"))
}

/// Enumerate every regular file under `root`, skipping any whose
/// forward-slashed relpath matches an exclude glob. Returns
/// `(relkey, abs_path, size)`; `relkey` is the snapshot's logical path —
/// portable, forward-slashed, relative to the source root. Iterative +
/// dependency-free (std `read_dir`); symlinks + specials are skipped,
/// unreadable directories are stepped over silently.
fn enumerate_source(root: &Path, excludes: &GlobSet) -> Vec<(String, PathBuf, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            let abs = entry.path();
            let Ok(rel) = abs.strip_prefix(root) else {
                continue;
            };
            let relkey = rel.to_string_lossy().replace('\\', "/");
            if ft.is_dir() {
                // Prune the whole subtree when the directory itself matches an
                // exclude (a bare `node_modules` / `.git` pattern), not just
                // its files — otherwise the bare form silently does nothing
                // and only `node_modules/**` would have worked.
                if relkey.is_empty() || !excludes.is_match(&relkey) {
                    stack.push(abs);
                }
                continue;
            }
            if !ft.is_file() {
                continue; // skip symlinks + specials
            }
            if relkey.is_empty() || excludes.is_match(&relkey) {
                continue;
            }
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            out.push((relkey, abs, size));
        }
    }
    out
}
