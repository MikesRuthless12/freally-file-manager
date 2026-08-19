//! Phase 45.2 — Tauri IPC for the named-queue / drag-merge / F2-mode
//! UX. Six commands, all backed by the `QueueRegistry` instance held
//! on `AppState`.
//!
//! Wire shape:
//!
//! - `queue_list() -> Vec<QueueSnapshotDto>` — one entry per queue
//!   currently held by the registry, with the badge count the
//!   Phase 45.3 tab strip uses.
//! - `queue_route_job(kind, src, dst) -> RoutedJobDto` — drop a new
//!   job into the right queue (auto-discriminating by physical drive
//!   id, or honouring F2-mode when set). Returns the assigned ids;
//!   actual execution is wired in a later sub-phase as the runner
//!   learns to consume every registry queue.
//! - `queue_merge(src_id, dst_id) -> Result<()>` — collapse two
//!   queues; emits `queue-merged` + `queue-removed` events.
//! - `queue_set_f2_mode(enabled)` — toggle the registry's
//!   `auto_enqueue_next` atomic. Transient — never persisted.
//! - `queue_pin_destination(label, path)` — append a tray
//!   destination target to `Settings::queue::pinned_destinations`
//!   and persist.
//! - `queue_get_pinned() -> Vec<PinnedDestinationDto>` — return the
//!   current pinned-destination list.

use std::sync::atomic::Ordering;

use freally_core::{JobKind, JobState, Queue, QueueId, QueueRegistry, QueueRegistryEvent};
use freally_settings::{AffinityGroupSetting, PinnedDestination};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Runtime};
use tokio::sync::broadcast::error::RecvError;

use crate::ipc_safety::{err_string, validate_ipc_path};
use crate::state::AppState;

/// Phase 45.7 follow-up — defensive caps on the pinned-destination
/// list. The renderer is part-trusted in the Tauri threat model, but
/// a buggy / forged `queue_pin_destination` storm would otherwise
/// pile up unbounded entries that the OS tray menu rebuild would
/// dutifully render.
const MAX_PINNED_DESTINATIONS: usize = 50;
const MAX_PINNED_LABEL_CHARS: usize = 64;
const MAX_PINNED_PATH_CHARS: usize = 1024;

/// Reject newlines / carriage returns / NUL / U+FFFD that would
/// corrupt OS tray menu rendering or signal a lossy
/// WTF-16 → UTF-8 coercion (see `ipc_safety.rs:30`).
fn pin_string_has_bad_chars(s: &str) -> bool {
    s.chars()
        .any(|c| matches!(c, '\n' | '\r' | '\0' | '\u{FFFD}'))
}

/// Tauri event names. Kept in one place so the JS side has a single
/// source of truth for the strings to `listen()` on.
pub const EVENT_QUEUE_ADDED: &str = "queue-added";
pub const EVENT_QUEUE_REMOVED: &str = "queue-removed";
pub const EVENT_QUEUE_MERGED: &str = "queue-merged";
pub const EVENT_QUEUE_JOB_ROUTED: &str = "queue-job-routed";

/// Wire shape for `queue-added` events.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueAddedEvent {
    pub id: u64,
    pub name: String,
}

/// Wire shape for `queue-removed` / `queue-merged` events.
/// `merged.src` populates `id` for `queue-removed`; the
/// `queue-merged` event carries both ids in `MergedEvent`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueIdEvent {
    pub id: u64,
}

/// Wire shape for `queue-merged` events.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueMergedEvent {
    pub src: u64,
    pub dst: u64,
}

/// Wire shape for `queue-job-routed` events.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueJobRoutedEvent {
    pub queue_id: u64,
    pub job_id: u64,
}

/// Wire shape used by the Phase 45.3 tab strip. Mirrors what the
/// frontend needs to render one tab per queue: identity (`id` /
/// `name`), the badge count (Pending + Running jobs), and a flag
/// for the F2-mode pulse animation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueSnapshotDto {
    /// Stable id (`QueueId::as_u64()`). Frontend keeps a
    /// `Map<id, tab>` keyed on this; ordering on the wire is
    /// insertion order.
    pub id: u64,
    /// Tab label — typically `"D: queue"` on Windows, `"default"`
    /// for the back-compat queue, `"queue N"` when no probe label
    /// is available.
    pub name: String,
    /// Number of `Pending` + `Running` jobs in this queue. Drives
    /// the badge counter in the tab strip.
    pub badge_count: usize,
    /// `true` when at least one job in this queue is currently
    /// `Running`. Phase 45.5 uses this to render the F2-mode pulse
    /// on whichever tab is the active routing target.
    pub running: bool,
}

/// Wire shape returned from [`queue_route_job`]. Frontend stashes
/// the ids on the JobList row so subsequent pause/resume/cancel
/// IPC calls can reference the right queue.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutedJobDto {
    /// Queue the job ended up in (matches a row from
    /// [`queue_list`]).
    pub queue_id: u64,
    /// Per-job id, unique across the whole registry.
    pub job_id: u64,
}

/// Wire shape for [`PinnedDestination`]. Mirrors the persisted form
/// 1:1 — kept distinct from the settings struct so adding a UI-only
/// field (e.g. a colour swatch in a future phase) doesn't churn
/// `freally-settings`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinnedDestinationDto {
    pub label: String,
    pub path: String,
}

impl From<PinnedDestination> for PinnedDestinationDto {
    fn from(p: PinnedDestination) -> Self {
        Self {
            label: p.label,
            path: p.path,
        }
    }
}

impl From<PinnedDestinationDto> for PinnedDestination {
    fn from(p: PinnedDestinationDto) -> Self {
        Self {
            label: p.label,
            path: p.path,
        }
    }
}

// ---------------------------------------------------------------------
// Tauri command thin wrappers — bodies live in the AppState helpers
// below so tests can exercise the logic without a real Tauri runtime.
// ---------------------------------------------------------------------

/// `queue_list()` — snapshot the registry. Read-only; never mutates.
#[tauri::command]
pub fn queue_list(state: tauri::State<'_, AppState>) -> Vec<QueueSnapshotDto> {
    queue_list_impl(state.inner())
}

/// `queue_route_job(kind, src, dst)` — enqueue a job through the
/// registry's auto-discriminating router. The wire-string `kind`
/// uses the same vocabulary as the legacy `start_copy` / job-list
/// surface (`"copy"`, `"move"`, `"delete"`, `"secure-delete"`,
/// `"verify"`).
#[tauri::command]
pub fn queue_route_job(
    state: tauri::State<'_, AppState>,
    kind: String,
    src: String,
    dst: Option<String>,
) -> Result<RoutedJobDto, String> {
    queue_route_job_impl(state.inner(), &kind, &src, dst.as_deref())
}

/// `queue_merge(src_id, dst_id)` — collapse two queues. No-op when
/// `src_id == dst_id`. Errors when either id doesn't resolve.
#[tauri::command]
pub fn queue_merge(
    state: tauri::State<'_, AppState>,
    src_id: u64,
    dst_id: u64,
) -> Result<(), String> {
    queue_merge_impl(state.inner(), src_id, dst_id)
}

/// `queue_set_f2_mode(enabled)` — flip the registry's
/// `auto_enqueue_next` flag. The frontend's F2 keybinding
/// invokes this. Transient state — never written to settings.
#[tauri::command]
pub fn queue_set_f2_mode(state: tauri::State<'_, AppState>, enabled: bool) {
    queue_set_f2_mode_impl(state.inner(), enabled);
}

/// `queue_pin_destination(label, path)` — append a row to
/// `Settings::queue::pinned_destinations` and persist. Duplicate
/// `(label, path)` pairs are ignored so a chatty UI can replay
/// the same call without growing the list. Phase 45.6 — calls
/// [`crate::rebuild_tray_menu`] on success so the OS tray menu
/// reflects the new row immediately.
#[tauri::command]
pub fn queue_pin_destination<R: Runtime>(
    state: tauri::State<'_, AppState>,
    app: AppHandle<R>,
    label: String,
    path: String,
) -> Result<Vec<PinnedDestinationDto>, String> {
    let result = queue_pin_destination_impl(state.inner(), &label, &path)?;
    if let Err(e) = crate::rebuild_tray_menu(&app) {
        eprintln!("[queue-pin] tray menu rebuild failed: {e}");
    }
    Ok(result)
}

/// `queue_get_pinned()` — return the current pinned-destination list.
#[tauri::command]
pub fn queue_get_pinned(state: tauri::State<'_, AppState>) -> Vec<PinnedDestinationDto> {
    queue_get_pinned_impl(state.inner())
}

/// `queue_unpin_destination(label, path)` — remove the matching row
/// from `Settings::queue::pinned_destinations` and persist. Returns
/// the post-removal list. Idempotent — removing a row that isn't
/// pinned is a no-op (returns the unchanged list). Phase 45.6 —
/// calls [`crate::rebuild_tray_menu`] on success.
#[tauri::command]
pub fn queue_unpin_destination<R: Runtime>(
    state: tauri::State<'_, AppState>,
    app: AppHandle<R>,
    label: String,
    path: String,
) -> Result<Vec<PinnedDestinationDto>, String> {
    let result = queue_unpin_destination_impl(state.inner(), &label, &path)?;
    if let Err(e) = crate::rebuild_tray_menu(&app) {
        eprintln!("[queue-unpin] tray menu rebuild failed: {e}");
    }
    Ok(result)
}

// ---------------------------------------------------------------------
// Test-friendly helpers — these are what the smoke test exercises.
// ---------------------------------------------------------------------

/// Implementation of [`queue_list`]. Public for tests.
pub fn queue_list_impl(state: &AppState) -> Vec<QueueSnapshotDto> {
    state
        .queues
        .queues()
        .into_iter()
        .map(|q| {
            let snap = q.snapshot();
            let badge_count = snap
                .iter()
                .filter(|j| matches!(j.state, JobState::Pending | JobState::Running))
                .count();
            let running = snap.iter().any(|j| j.state == JobState::Running);
            QueueSnapshotDto {
                id: q.id().as_u64(),
                name: q.name().to_string(),
                badge_count,
                running,
            }
        })
        .collect()
}

/// Implementation of [`queue_route_job`]. Public for tests.
///
/// Phase 45.7 follow-up — gate `src` and `dst` through the standing
/// Phase 17e IPC path validator (rejects `..` traversal, NUL bytes,
/// U+FFFD-poisoned strings, and empty-after-trim). Same gate every
/// other path-typed command flows through; matches the contract
/// documented in `ipc_safety.rs:11`.
pub fn queue_route_job_impl(
    state: &AppState,
    kind: &str,
    src: &str,
    dst: Option<&str>,
) -> Result<RoutedJobDto, String> {
    let kind = job_kind_from_wire(kind)?;
    let src = validate_ipc_path(src).map_err(err_string)?;
    let dst = match dst {
        Some(d) => Some(validate_ipc_path(d).map_err(err_string)?),
        None => None,
    };
    let (qid, jid, _control) = state.queues.route(kind, src, dst);
    Ok(RoutedJobDto {
        queue_id: qid.as_u64(),
        job_id: jid.as_u64(),
    })
}

/// Implementation of [`queue_merge`]. Public for tests.
pub fn queue_merge_impl(state: &AppState, src_id: u64, dst_id: u64) -> Result<(), String> {
    state
        .queues
        .merge_into(QueueId::from_u64(src_id), QueueId::from_u64(dst_id))
        .map_err(|e| e.to_string())
}

/// Implementation of [`queue_set_f2_mode`]. Public for tests.
pub fn queue_set_f2_mode_impl(state: &AppState, enabled: bool) {
    state
        .queues
        .auto_enqueue_next
        .store(enabled, Ordering::Relaxed);
}

/// Implementation of [`queue_pin_destination`]. Public for tests.
///
/// Phase 45.7 follow-up — defense-in-depth at the IPC boundary:
///   * Trim, then reject empty.
///   * Reject control chars (`\n`, `\r`, `\0`) and U+FFFD that
///     would corrupt OS tray menu rendering or signal a lossy
///     UTF-16 coercion.
///   * Cap label at [`MAX_PINNED_LABEL_CHARS`] and path at
///     [`MAX_PINNED_PATH_CHARS`] so a forged caller can't blow out
///     the menu width or settings.toml size.
///   * Cap the persisted list at [`MAX_PINNED_DESTINATIONS`].
///
/// The path is intentionally NOT routed through
/// [`crate::ipc_safety::validate_ipc_path`] — pinned destinations
/// may be Phase 32 backend URIs (e.g. `s3://bucket/inbox`,
/// `sftp://host/path`) which the lexical traversal gate would
/// flag because of their `://` and `..`-resembling segments. The
/// checks above cover the corruption surface without rejecting
/// legitimate URIs.
pub fn queue_pin_destination_impl(
    state: &AppState,
    label: &str,
    path: &str,
) -> Result<Vec<PinnedDestinationDto>, String> {
    let label = label.trim().to_string();
    let path = path.trim().to_string();
    if label.is_empty() {
        return Err("err-pinned-destination-label-empty".to_string());
    }
    if path.is_empty() {
        return Err("err-pinned-destination-path-empty".to_string());
    }
    if label.chars().count() > MAX_PINNED_LABEL_CHARS {
        return Err("err-pinned-destination-label-too-long".to_string());
    }
    if path.chars().count() > MAX_PINNED_PATH_CHARS {
        return Err("err-pinned-destination-path-too-long".to_string());
    }
    if pin_string_has_bad_chars(&label) {
        return Err("err-pinned-destination-label-invalid".to_string());
    }
    if pin_string_has_bad_chars(&path) {
        return Err("err-pinned-destination-path-invalid".to_string());
    }
    let entry = PinnedDestination { label, path };
    // Recover the inner guard if the RwLock was poisoned by a prior
    // panic — see the lock-poisoning policy comment at the top of
    // `crates/freally-core/src/queue.rs`. Pin/unpin keeping working
    // after an unrelated mid-write panic is a much better outcome
    // than every subsequent IPC call returning a stuck error.
    let mut s = state.settings.write().unwrap_or_else(|p| p.into_inner());
    if !s.queue.pinned_destinations.iter().any(|p| p == &entry) {
        if s.queue.pinned_destinations.len() >= MAX_PINNED_DESTINATIONS {
            return Err("err-pinned-destination-too-many".to_string());
        }
        s.queue.pinned_destinations.push(entry);
    }
    state.persist_settings(&s)?;
    Ok(s.queue
        .pinned_destinations
        .iter()
        .cloned()
        .map(PinnedDestinationDto::from)
        .collect())
}

/// Implementation of [`queue_get_pinned`]. Public for tests.
pub fn queue_get_pinned_impl(state: &AppState) -> Vec<PinnedDestinationDto> {
    state
        .settings_snapshot()
        .queue
        .pinned_destinations
        .into_iter()
        .map(PinnedDestinationDto::from)
        .collect()
}

/// Implementation of [`queue_unpin_destination`]. Public for tests.
pub fn queue_unpin_destination_impl(
    state: &AppState,
    label: &str,
    path: &str,
) -> Result<Vec<PinnedDestinationDto>, String> {
    let label = label.trim().to_string();
    let path = path.trim().to_string();
    let target = PinnedDestination { label, path };
    // Recover the inner guard if the RwLock was poisoned by a prior
    // panic — see the lock-poisoning policy comment at the top of
    // `crates/freally-core/src/queue.rs`. Pin/unpin keeping working
    // after an unrelated mid-write panic is a much better outcome
    // than every subsequent IPC call returning a stuck error.
    let mut s = state.settings.write().unwrap_or_else(|p| p.into_inner());
    s.queue.pinned_destinations.retain(|p| p != &target);
    state.persist_settings(&s)?;
    Ok(s.queue
        .pinned_destinations
        .iter()
        .cloned()
        .map(PinnedDestinationDto::from)
        .collect())
}

// ---------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------

fn job_kind_from_wire(s: &str) -> Result<JobKind, String> {
    Ok(match s {
        "copy" => JobKind::Copy,
        "move" => JobKind::Move,
        "delete" => JobKind::Delete,
        "secure-delete" => JobKind::SecureDelete,
        "verify" => JobKind::Verify,
        other => return Err(format!("unknown job kind: {other:?}")),
    })
}

// =====================================================================
// FFM-M18 — queue-affinity + concurrency overrides
// =====================================================================

/// Defensive caps: each group spawns a queue and each prefix is
/// consulted on every route, so an unbounded list from a forged
/// renderer call would be a routing-path DoS.
const MAX_AFFINITY_GROUPS: usize = 32;
const MAX_AFFINITY_PREFIXES: usize = 32;
const MAX_AFFINITY_NAME_CHARS: usize = 48;
/// The engine's own tree-concurrency ceiling; mirrors
/// `ConcurrencyChoice::resolved`'s clamp.
const MAX_QUEUE_WORKERS: u32 = 16;

/// Wire shape for one affinity group.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AffinityGroupDto {
    /// Queue name shown in the tab strip.
    pub name: String,
    /// Path prefixes claimed by this group.
    pub prefixes: Vec<String>,
    /// Worker count; `0` inherits the global setting, `1` is HDD-mode.
    pub workers: u32,
}

impl From<&AffinityGroupSetting> for AffinityGroupDto {
    fn from(g: &AffinityGroupSetting) -> Self {
        Self {
            name: g.name.clone(),
            prefixes: g.prefixes.clone(),
            workers: g.workers,
        }
    }
}

/// Validate a list of groups from the renderer.
///
/// Every prefix goes through the standing Phase 17e IPC gate, so a
/// traversal-laden or U+FFFD-poisoned prefix can never end up steering
/// which physical queue a copy lands in.
pub fn validate_affinity_groups(
    dtos: &[AffinityGroupDto],
) -> Result<Vec<AffinityGroupSetting>, String> {
    if dtos.len() > MAX_AFFINITY_GROUPS {
        return Err("err-affinity-too-many".to_string());
    }
    let mut out = Vec::with_capacity(dtos.len());
    let mut seen: Vec<String> = Vec::new();
    for dto in dtos {
        let name = dto.name.trim();
        if name.is_empty() || name.chars().count() > MAX_AFFINITY_NAME_CHARS {
            return Err("err-affinity-name-invalid".to_string());
        }
        if pin_string_has_bad_chars(name) {
            return Err("err-affinity-name-invalid".to_string());
        }
        if seen.iter().any(|s| s == name) {
            // Bucket ids are derived from the name, so two groups
            // sharing one would silently merge and the user would see
            // an override they never asked for.
            return Err("err-affinity-name-duplicate".to_string());
        }
        seen.push(name.to_string());

        if dto.prefixes.is_empty() || dto.prefixes.len() > MAX_AFFINITY_PREFIXES {
            return Err("err-affinity-prefixes-invalid".to_string());
        }
        let mut prefixes = Vec::with_capacity(dto.prefixes.len());
        for raw in &dto.prefixes {
            let p = validate_ipc_path(raw).map_err(err_string)?;
            prefixes.push(p.to_string_lossy().into_owned());
        }
        if dto.workers > MAX_QUEUE_WORKERS {
            return Err("err-affinity-workers-invalid".to_string());
        }
        out.push(AffinityGroupSetting {
            name: name.to_string(),
            prefixes,
            workers: dto.workers,
        });
    }
    Ok(out)
}

/// Translate persisted groups into the engine's shape.
pub fn to_core_groups(groups: &[AffinityGroupSetting]) -> Vec<freally_core::AffinityGroup> {
    groups
        .iter()
        .map(|g| freally_core::AffinityGroup {
            name: g.name.clone(),
            prefixes: g.prefixes.iter().map(std::path::PathBuf::from).collect(),
            // `0` means "inherit the global setting"; the engine's
            // `Option` says the same thing without a magic number.
            workers: (g.workers > 0).then_some(g.workers),
        })
        .collect()
}

/// `queue_get_affinity()` — the groups currently in force.
#[tauri::command]
pub fn queue_get_affinity(state: tauri::State<'_, AppState>) -> Vec<AffinityGroupDto> {
    state
        .settings_snapshot()
        .queue
        .affinity_groups
        .iter()
        .map(AffinityGroupDto::from)
        .collect()
}

/// `queue_set_affinity(groups)` — replace the whole list, persist it,
/// and apply it to the live registry.
#[tauri::command]
pub fn queue_set_affinity(
    state: tauri::State<'_, AppState>,
    groups: Vec<AffinityGroupDto>,
) -> Result<Vec<AffinityGroupDto>, String> {
    queue_set_affinity_impl(state.inner(), &groups)
}

/// Implementation of [`queue_set_affinity`]. Public for tests.
pub fn queue_set_affinity_impl(
    state: &AppState,
    groups: &[AffinityGroupDto],
) -> Result<Vec<AffinityGroupDto>, String> {
    let validated = validate_affinity_groups(groups)?;
    state.queues.set_affinity_groups(to_core_groups(&validated));
    let mut s = state.settings.write().unwrap_or_else(|p| p.into_inner());
    s.queue.affinity_groups = validated;
    state.persist_settings(&s)?;
    Ok(s.queue
        .affinity_groups
        .iter()
        .map(AffinityGroupDto::from)
        .collect())
}

/// Push the persisted affinity groups into the live registry.
///
/// Called at startup — without it, a restart would silently drop every
/// override until the user reopened the Queue settings pane.
pub fn apply_persisted_affinity(state: &AppState) {
    let groups = state.settings_snapshot().queue.affinity_groups;
    state.queues.set_affinity_groups(to_core_groups(&groups));
}

// =====================================================================
// FFM-M19 — per-job priority, reorder, and queue move
// =====================================================================

// There is deliberately no `queue_run_next` or `queue_reorder_job`
// here. `commands.rs::enqueue_jobs` spawns a runner for every source
// immediately and `run_job` calls `queue.start(id)` as its first act,
// so no job is ever `Pending` and queue order is display-only. Both
// commands would have reported success while changing nothing the user
// could observe. Re-shipping them needs a real per-queue permit pool
// that starts jobs in queue order.

/// `queue_move_job(job_id, dst_queue_id)` — move one pending job to a
/// different drive queue.
#[tauri::command]
pub fn queue_move_job(
    state: tauri::State<'_, AppState>,
    job_id: u64,
    dst_queue_id: u64,
) -> Result<(), String> {
    state
        .queues
        .move_job_to_queue(
            freally_core::JobId::from_u64(job_id),
            QueueId::from_u64(dst_queue_id),
        )
        .map_err(|e| e.to_string())
}

/// Resolve a queue from the raw id the IPC boundary hands us.
///
/// Both boost commands do this lookup and both must fail with the same
/// Fluent key, so the key lives in exactly one place.
fn queue_of(state: &AppState, queue_id: u64) -> Result<Queue, &'static str> {
    state
        .queues
        .get(QueueId::from_u64(queue_id))
        .ok_or("err-queue-unknown")
}

/// `queue_boost_job(queue_id, job_id)` — pause every running sibling so
/// this job gets the bandwidth. Returns the ids paused, which the
/// frontend hands back to [`queue_clear_boost`] so exactly those resume.
#[tauri::command]
pub fn queue_boost_job(
    state: tauri::State<'_, AppState>,
    queue_id: u64,
    job_id: u64,
) -> Result<Vec<u64>, String> {
    let queue = queue_of(&state, queue_id)?;
    Ok(queue
        .boost_job(freally_core::JobId::from_u64(job_id))
        .into_iter()
        .map(|id| id.as_u64())
        .collect())
}

/// `queue_clear_boost(queue_id, paused)` — resume exactly the jobs a
/// boost paused.
///
/// Takes the id list rather than "resume everything" so a job the user
/// had paused by hand before the boost stays paused.
#[tauri::command]
pub fn queue_clear_boost(
    state: tauri::State<'_, AppState>,
    queue_id: u64,
    paused: Vec<u64>,
) -> Result<(), String> {
    let queue = queue_of(&state, queue_id)?;
    let ids: Vec<freally_core::JobId> = paused
        .into_iter()
        .map(freally_core::JobId::from_u64)
        .collect();
    queue.clear_boost(&ids);
    Ok(())
}

/// Subscribe to the registry's broadcast channel and forward every
/// [`QueueRegistryEvent`] to the matching Tauri event so the
/// frontend can react without polling. Returns the spawned
/// `JoinHandle`; callers store it on the runtime so the task
/// outlives the setup hook.
///
/// On `Lagged` (subscriber too slow to keep up with the bounded
/// channel) the pump skips silently and resyncs on the next event:
/// the frontend's reconciliation path is `queue_list()`, which
/// every tab-strip refresh already calls.
pub fn spawn_registry_event_pump(
    app: AppHandle,
    registry: QueueRegistry,
) -> tauri::async_runtime::JoinHandle<()> {
    let mut rx = registry.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(QueueRegistryEvent::QueueAdded { id, name }) => {
                    let _ = app.emit(
                        EVENT_QUEUE_ADDED,
                        QueueAddedEvent {
                            id: id.as_u64(),
                            name: name.to_string(),
                        },
                    );
                }
                Ok(QueueRegistryEvent::QueueRemoved { id }) => {
                    let _ = app.emit(EVENT_QUEUE_REMOVED, QueueIdEvent { id: id.as_u64() });
                }
                Ok(QueueRegistryEvent::QueueMerged { src, dst }) => {
                    let _ = app.emit(
                        EVENT_QUEUE_MERGED,
                        QueueMergedEvent {
                            src: src.as_u64(),
                            dst: dst.as_u64(),
                        },
                    );
                }
                Ok(QueueRegistryEvent::JobRouted { queue_id, job_id }) => {
                    let _ = app.emit(
                        EVENT_QUEUE_JOB_ROUTED,
                        QueueJobRoutedEvent {
                            queue_id: queue_id.as_u64(),
                            job_id: job_id.as_u64(),
                        },
                    );
                }
                Err(RecvError::Lagged(_)) => {
                    // Subscriber fell behind. Frontend's tab-strip
                    // reconcile path is `queue_list()`; resyncing
                    // there is cheaper than buffering events.
                    continue;
                }
                Err(RecvError::Closed) => break,
            }
        }
    })
}
