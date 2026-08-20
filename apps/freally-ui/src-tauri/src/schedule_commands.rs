//! FFM-M17 — Tauri IPC for the scheduler UI.
//!
//! Phase 14d could only *render* a scheduler stanza for the user to
//! paste in by hand. These commands install it: create, list, edit, and
//! remove user-level scheduled runs through
//! [`freally_platform::scheduler`], with a next-run preview and a
//! missed-run policy.
//!
//! ## What a schedule actually runs
//!
//! Always **this app's own binary**, invoked with the already-shipped
//! shell-integration CLI:
//!
//! ```text
//! <app-exe> --enqueue <copy|move> --files-from <manifest> --destination <dst> [--relative-to <root>]
//! ```
//!
//! The frontend never supplies a program or an argv — it supplies a
//! manifest, a destination, and a trigger. That is deliberate: a
//! scheduled task is a command line the OS runs unattended, so letting
//! a forged IPC call name the program would turn this into a
//! persistence primitive. Reusing the `--enqueue` entry point also
//! means a scheduled run takes the same validated, journaled engine
//! path as a drag-and-drop.
//!
//! ## Registry
//!
//! `Settings::schedules` is the app's record; the OS holds the
//! authoritative artifact. [`schedule_list`] reports both — `installed`
//! comes from probing the OS, so a schedule deleted behind the app's
//! back (by Task Scheduler's own UI, or a cleanup tool) shows as
//! missing instead of being silently claimed as live.

use freally_platform::scheduler::{
    MissedRunPolicy, ScheduleTrigger, ScheduledJob, next_run_after, policy_is_honored, validate_id,
};
use freally_settings::ScheduleEntry;
use serde::{Deserialize, Serialize};

use crate::ipc_safety::{err_string, validate_ipc_path};
use crate::state::AppState;

/// Defensive cap on the number of schedules. Each one is an OS
/// artifact; an unbounded list would be a way to spam the host's
/// scheduler from a forged renderer call.
const MAX_SCHEDULES: usize = 50;
const MAX_LABEL_CHARS: usize = 64;

/// Wire shape for one schedule, plus the two derived fields the UI
/// needs and cannot compute itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleDto {
    /// Stable slug — `[a-z0-9-]{1,48}`.
    pub id: String,
    /// User-facing label.
    pub label: String,
    /// `"copy"` or `"move"`.
    pub verb: String,
    /// Manifest listing the source set (FFM-M13 formats).
    pub files_from: String,
    /// Destination root.
    pub destination: String,
    /// Optional root whose structure is preserved under the
    /// destination. Empty = flatten to basenames.
    pub relative_to: String,
    /// `"hourly"` | `"daily"` | `"weekly"`.
    pub trigger: String,
    /// 0 = Sunday … 6 = Saturday. Only meaningful for `"weekly"`.
    pub weekday: u32,
    /// Hour of day. Ignored for `"hourly"`.
    pub hour: u32,
    /// Minutes past the hour.
    pub minute: u32,
    /// Run a firing missed while the machine was off.
    pub run_when_available: bool,
    /// Whether the OS currently holds the matching artifact. `false`
    /// means the app knows about a schedule the scheduler does not.
    #[serde(default)]
    pub installed: bool,
    /// Next firing, as Unix seconds. `None` when the trigger could not
    /// be interpreted.
    #[serde(default)]
    pub next_run_unix_secs: Option<i64>,
    /// Whether this host honours the requested missed-run policy.
    /// `false` on macOS with `Skip` — launchd always runs a missed
    /// calendar job at wake, so the UI must not promise otherwise.
    #[serde(default)]
    pub missed_run_honored: bool,
}

impl ScheduleDto {
    /// Build the wire DTO for a persisted entry.
    ///
    /// The three derived fields are parameters rather than defaults
    /// because none of them can be read off the entry — they come from
    /// the OS probe and the clock, and the only caller
    /// ([`decorate`]) always has them. Defaulting them here would put a
    /// DTO into a state no consumer ever legitimately sees.
    fn new(
        e: &ScheduleEntry,
        installed: bool,
        next_run_unix_secs: Option<i64>,
        missed_run_honored: bool,
    ) -> Self {
        Self {
            id: e.id.clone(),
            label: e.label.clone(),
            verb: e.verb.clone(),
            files_from: e.files_from.clone(),
            destination: e.destination.clone(),
            relative_to: e.relative_to.clone(),
            trigger: e.trigger.clone(),
            weekday: e.weekday,
            hour: e.hour,
            minute: e.minute,
            run_when_available: e.run_when_available,
            installed,
            next_run_unix_secs,
            missed_run_honored,
        }
    }
}

/// Translate a persisted entry's trigger fields into the platform
/// enum. `None` when the stored trigger name is unknown — a settings
/// file written by a newer build should not crash an older one.
fn trigger_of(entry: &ScheduleEntry) -> Option<ScheduleTrigger> {
    match entry.trigger.as_str() {
        "hourly" => Some(ScheduleTrigger::Hourly {
            minute: entry.minute,
        }),
        "daily" => Some(ScheduleTrigger::Daily {
            hour: entry.hour,
            minute: entry.minute,
        }),
        "weekly" => Some(ScheduleTrigger::Weekly {
            weekday: entry.weekday,
            hour: entry.hour,
            minute: entry.minute,
        }),
        _ => None,
    }
}

fn policy_of(entry: &ScheduleEntry) -> MissedRunPolicy {
    if entry.run_when_available {
        MissedRunPolicy::RunWhenAvailable
    } else {
        MissedRunPolicy::Skip
    }
}

/// Seconds this machine is ahead of UTC, right now.
///
/// The platform's `next_run_after` works in local wall-clock seconds
/// with no timezone database; the caller supplies the offset. Reading
/// it here (rather than baking it into the platform crate) keeps that
/// function pure and exhaustively testable.
fn utc_offset_secs() -> i64 {
    offset_at_unix(now_unix_secs())
}

/// Seconds this machine is ahead of UTC **at `unix_secs`**, according
/// to the OS timezone database.
///
/// Used as the `offset_at` resolver in production. Kept separate from
/// [`next_run_for`] so that function stays pure.
fn offset_at_unix(unix_secs: i64) -> i64 {
    use chrono::{DateTime, Offset, TimeZone};
    let Some(utc) = DateTime::from_timestamp(unix_secs, 0) else {
        return 0;
    };
    i64::from(
        chrono::Local
            .from_utc_datetime(&utc.naive_utc())
            .offset()
            .fix()
            .local_minus_utc(),
    )
}

/// Compute the next firing as Unix seconds.
///
/// `utc_offset` places *now* on the local wall clock; `offset_at` gives
/// the offset in effect at an arbitrary instant, and is what turns the
/// computed wall-clock firing back into a real instant.
///
/// The two are not interchangeable. Converting the firing back with
/// **today's** offset is wrong for the whole week before a DST
/// transition: on 2026-03-07 in America/New_York a "Sunday 03:00" job
/// rendered as "Next run: 04:00", so the app contradicted the schedule
/// it had itself installed.
///
/// The loop below solves `t + offset_at(t) == next_local` — one step is
/// enough for a standard transition, the second confirms convergence.
pub fn next_run_for(
    entry: &ScheduleEntry,
    now_unix_secs: i64,
    utc_offset: i64,
    offset_at: impl Fn(i64) -> i64,
) -> Option<i64> {
    let trigger = trigger_of(entry)?;
    let next_local = next_run_after(trigger, now_unix_secs + utc_offset);
    let mut t = next_local - utc_offset;
    for _ in 0..2 {
        let settled = next_local - offset_at(t);
        if settled == t {
            break;
        }
        t = settled;
    }
    Some(t)
}

/// Build the OS-level job for `entry`.
///
/// The program is [`std::env::current_exe`] — never anything the
/// caller named.
fn scheduled_job_for(entry: &ScheduleEntry) -> Result<ScheduledJob, String> {
    let program = std::env::current_exe().map_err(|_| "err-schedule-no-program".to_string())?;
    let mut args = vec![
        "--enqueue".to_string(),
        entry.verb.clone(),
        "--files-from".to_string(),
        entry.files_from.clone(),
        "--destination".to_string(),
        entry.destination.clone(),
    ];
    if !entry.relative_to.is_empty() {
        args.push("--relative-to".to_string());
        args.push(entry.relative_to.clone());
    }
    Ok(ScheduledJob {
        id: entry.id.clone(),
        label: entry.label.clone(),
        program,
        args,
        trigger: trigger_of(entry).ok_or("err-schedule-trigger-invalid")?,
        missed_run: policy_of(entry),
    })
}

/// Mint an id for a new schedule: a `[a-z0-9-]` slug of the label,
/// uniquified against `existing`.
///
/// Two schedules can legitimately share a label — "Nightly backup" for
/// two different file lists — so a bare slug would collide, and
/// `schedule_save`'s upsert would then *replace* the first schedule
/// instead of adding a second, leaving one OS artifact where the user
/// asked for two. Minting backend-side also means the renderer cannot
/// guess an id that is already taken.
fn mint_schedule_id(label: &str, existing: &[ScheduleEntry]) -> String {
    let stem: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(40)
        .collect();
    let stem = if stem.is_empty() {
        "schedule".to_string()
    } else {
        stem
    };
    if !existing.iter().any(|e| e.id == stem) {
        return stem;
    }
    // Bounded by the list length: with `n` existing entries, one of
    // `stem-2 ..= stem-(n+2)` is always free.
    for n in 2..=(existing.len() as u32 + 2) {
        let candidate = format!("{stem}-{n}");
        if !existing.iter().any(|e| e.id == candidate) {
            return candidate;
        }
    }
    unreachable!("a free suffix exists within existing.len() + 1 candidates")
}

/// Validate an incoming DTO and normalise it into a persistable entry.
///
/// A **blank** `dto.id` means "create" and the backend mints a unique
/// one from the label; a non-blank id is an explicit edit of that row
/// and is validated as-is. Every path goes through the standing Phase
/// 17e IPC gate (`..` traversal, NUL, U+FFFD), and the label / verb /
/// trigger are checked before anything reaches a scheduler stanza.
pub fn entry_from_dto(
    dto: &ScheduleDto,
    existing: &[ScheduleEntry],
) -> Result<ScheduleEntry, String> {
    let label = dto.label.trim();
    if label.is_empty()
        || label.chars().count() > MAX_LABEL_CHARS
        || label.chars().any(char::is_control)
    {
        return Err("err-schedule-label-invalid".to_string());
    }

    let id = if dto.id.trim().is_empty() {
        mint_schedule_id(label, existing)
    } else {
        validate_id(dto.id.trim()).map_err(|_| "err-schedule-id-invalid".to_string())?;
        dto.id.trim().to_string()
    };

    if !matches!(dto.verb.as_str(), "copy" | "move") {
        return Err("err-schedule-verb-invalid".to_string());
    }
    if !matches!(dto.trigger.as_str(), "hourly" | "daily" | "weekly") {
        return Err("err-schedule-trigger-invalid".to_string());
    }
    if dto.hour > 23 || dto.minute > 59 || dto.weekday > 6 {
        return Err("err-schedule-trigger-invalid".to_string());
    }

    let files_from = validate_ipc_path(&dto.files_from).map_err(err_string)?;
    let destination = validate_ipc_path(&dto.destination).map_err(err_string)?;
    let relative_to = if dto.relative_to.trim().is_empty() {
        String::new()
    } else {
        validate_ipc_path(&dto.relative_to)
            .map_err(err_string)?
            .to_string_lossy()
            .into_owned()
    };

    Ok(ScheduleEntry {
        id,
        label: label.to_string(),
        verb: dto.verb.clone(),
        files_from: files_from.to_string_lossy().into_owned(),
        destination: destination.to_string_lossy().into_owned(),
        relative_to,
        trigger: dto.trigger.clone(),
        weekday: dto.weekday,
        hour: dto.hour,
        minute: dto.minute,
        run_when_available: dto.run_when_available,
    })
}

/// Decorate persisted entries with the derived fields, using an
/// injected clock + OS probe so the whole shape is testable.
pub fn decorate(
    entries: &[ScheduleEntry],
    now_unix_secs: i64,
    utc_offset: i64,
    offset_at: impl Fn(i64) -> i64,
    installed: impl Fn(&str) -> bool,
) -> Vec<ScheduleDto> {
    entries
        .iter()
        .map(|e| {
            ScheduleDto::new(
                e,
                installed(&e.id),
                next_run_for(e, now_unix_secs, utc_offset, &offset_at),
                policy_is_honored(policy_of(e)),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------

/// `schedule_list()` — every schedule this install has created, with
/// its live OS status and next-run preview.
#[tauri::command(async)]
pub fn schedule_list(state: tauri::State<'_, AppState>) -> Vec<ScheduleDto> {
    schedule_list_impl(state.inner())
}

/// `schedule_save(dto)` — create or replace one schedule, installing
/// the OS artifact. Returns the full post-save list.
#[tauri::command(async)]
pub fn schedule_save(
    state: tauri::State<'_, AppState>,
    dto: ScheduleDto,
) -> Result<Vec<ScheduleDto>, String> {
    schedule_save_impl(state.inner(), &dto)
}

/// `schedule_remove(id)` — uninstall the OS artifact and forget the
/// entry. Idempotent.
#[tauri::command(async)]
pub fn schedule_remove(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<Vec<ScheduleDto>, String> {
    schedule_remove_impl(state.inner(), &id)
}

// ---------------------------------------------------------------------
// Implementations — public for tests
// ---------------------------------------------------------------------

fn now_unix_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Implementation of [`schedule_list`]. Public for tests.
pub fn schedule_list_impl(state: &AppState) -> Vec<ScheduleDto> {
    decorate_live(&state.settings_snapshot().schedules.entries)
}

/// Decorate `entries` against the real clock and the real OS scheduler.
///
/// The injectable [`decorate`] stays public so the OS probe can be
/// stubbed in tests; this is the one production spelling of it.
fn decorate_live(entries: &[ScheduleEntry]) -> Vec<ScheduleDto> {
    // One enumeration, not one probe per row: on Windows each per-id
    // probe spawns `schtasks /Query` (~29 ms), so a ten-row list cost a
    // quarter-second of blocked UI. `installed_ids` is a single
    // folder-scoped call regardless of row count.
    let installed = freally_platform::scheduler::installed_ids().unwrap_or_default();
    decorate(
        entries,
        now_unix_secs(),
        utc_offset_secs(),
        offset_at_unix,
        |id| installed.contains(id),
    )
}

/// Implementation of [`schedule_save`]. Public for tests.
///
/// Persists **before** installing, and rolls the settings row back if
/// the install fails.
///
/// The opposite order was chosen originally to avoid the mirror-image
/// orphan — a settings row for a task the OS rejected. But one of the
/// two has to be transactional, and the OS artifact is the one that
/// does damage unattended: `schtasks /Create` succeeding and then
/// `save_to` failing (write-protected stick, disk full) left a task
/// firing forever that no later run could even see, because the
/// settings no longer contained it — no `schedule_list` row, no UI
/// affordance to remove it. A stale settings row, by contrast, is
/// visible, reads as "Missing from the system scheduler", and can be
/// removed from the panel.
pub fn schedule_save_impl(state: &AppState, dto: &ScheduleDto) -> Result<Vec<ScheduleDto>, String> {
    // FFM-M21 — a schedule bakes `current_exe()` into host-global OS
    // state. On a portable install that path is on the removable
    // stick: unplug it and the host fires a task pointing at nothing,
    // while the app that could have removed it left with the stick.
    // `shell_commands` and `autostart` already honour this gate;
    // `schedule_save` did not.
    if !freally_settings::portable::allows_os_integration() {
        return Err("err-schedule-portable".to_string());
    }
    // The write lock is taken **first** and held across
    // validate → cap-check → install → push. This command is
    // `#[tauri::command(async)]`, so N forged invokes run concurrently;
    // checking the cap against a snapshot and then dropping the lock let
    // every one of them see the same under-cap list and install an OS
    // artifact, blowing straight past `MAX_SCHEDULES`. (This is the
    // shape `favorites_save_impl` already had.)
    let mut s = state.settings.write().unwrap_or_else(|p| p.into_inner());
    let entry = entry_from_dto(dto, &s.schedules.entries)?;
    let is_new = !s.schedules.entries.iter().any(|e| e.id == entry.id);
    if is_new && s.schedules.entries.len() >= MAX_SCHEDULES {
        return Err("err-schedule-too-many".to_string());
    }

    let job = scheduled_job_for(&entry)?;

    let previous = s.schedules.entries.clone();
    match s.schedules.entries.iter_mut().find(|e| e.id == entry.id) {
        Some(existing) => *existing = entry.clone(),
        None => s.schedules.entries.push(entry.clone()),
    }
    // Roll the in-memory mutation back if the write fails, exactly as
    // the install path below does. A bare `?` here left the row live in
    // the shared `Settings` after telling the caller the save failed —
    // so `schedule_list` rendered it as "Missing from the system
    // scheduler", it never fired, and the next successful settings write
    // from any unrelated command (`update_settings`, favorites, …)
    // persisted the whole struct, permanently writing the row the user
    // was told had failed.
    if let Err(e) = state.persist_settings(&s) {
        s.schedules.entries = previous;
        return Err(e);
    }

    if let Err(e) = freally_platform::scheduler::install(&job) {
        tracing::warn!(target: "freally::schedule", id = %entry.id, error = %e, "install failed");
        // Undo the row we just wrote, so a rejected install leaves no
        // trace rather than a phantom schedule.
        s.schedules.entries = previous;
        let _ = state.persist_settings(&s);
        return Err(format!("err-schedule-install-failed: {e}"));
    }
    Ok(decorate_live(&s.schedules.entries))
}

/// Implementation of [`schedule_remove`]. Public for tests.
///
/// Uninstalls first, then forgets. The other order would drop the only
/// record of an artifact that is still installed and still firing.
pub fn schedule_remove_impl(state: &AppState, id: &str) -> Result<Vec<ScheduleDto>, String> {
    validate_id(id).map_err(|_| "err-schedule-id-invalid".to_string())?;
    freally_platform::scheduler::remove(id).map_err(|e| {
        tracing::warn!(target: "freally::schedule", id = %id, error = %e, "remove failed");
        format!("err-schedule-remove-failed: {e}")
    })?;
    let mut s = state.settings.write().unwrap_or_else(|p| p.into_inner());
    s.schedules.entries.retain(|e| e.id != id);
    state.persist_settings(&s)?;
    Ok(decorate_live(&s.schedules.entries))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dto() -> ScheduleDto {
        ScheduleDto {
            id: "nightly-photos".to_string(),
            label: "Nightly photos".to_string(),
            verb: "copy".to_string(),
            files_from: if cfg!(windows) {
                r"C:\lists\photos.txt".to_string()
            } else {
                "/lists/photos.txt".to_string()
            },
            destination: if cfg!(windows) {
                r"D:\backup".to_string()
            } else {
                "/backup".to_string()
            },
            relative_to: String::new(),
            trigger: "daily".to_string(),
            weekday: 0,
            hour: 3,
            minute: 30,
            run_when_available: true,
            installed: false,
            next_run_unix_secs: None,
            missed_run_honored: true,
        }
    }

    #[test]
    fn a_valid_dto_normalises_into_an_entry() {
        let entry = entry_from_dto(&dto(), &[]).expect("valid");
        assert_eq!(entry.id, "nightly-photos");
        assert_eq!(entry.verb, "copy");
        assert_eq!(entry.hour, 3);
        assert!(entry.run_when_available);
    }

    #[test]
    fn a_hostile_id_is_refused_before_any_os_call() {
        for bad in ["../evil", "Nightly", "night ly", &"a".repeat(49)] {
            let mut d = dto();
            d.id = bad.to_string();
            assert_eq!(
                entry_from_dto(&d, &[]).unwrap_err(),
                "err-schedule-id-invalid",
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn a_blank_id_means_create_and_mints_a_unique_one() {
        let mut d = dto();
        d.id = String::new();
        let first = entry_from_dto(&d, &[]).expect("blank id is a create");
        assert_eq!(first.id, "nightly-photos");

        // Two schedules may legitimately share a label. Without the
        // uniquifier the second save would upsert *over* the first,
        // leaving one OS artifact where the user asked for two.
        let second = entry_from_dto(&d, std::slice::from_ref(&first)).unwrap();
        assert_eq!(second.id, "nightly-photos-2");
        let third = entry_from_dto(&d, &[first, second]).unwrap();
        assert_eq!(third.id, "nightly-photos-3");
    }

    #[test]
    fn a_label_with_no_usable_characters_still_mints_an_id() {
        let mut d = dto();
        d.id = String::new();
        d.label = "!!!".to_string();
        assert_eq!(entry_from_dto(&d, &[]).unwrap().id, "schedule");
    }

    #[test]
    fn a_non_blank_id_is_an_edit_and_is_kept_verbatim() {
        let existing = entry_from_dto(&dto(), &[]).unwrap();
        let mut d = dto();
        d.label = "Renamed".to_string();
        let edited = entry_from_dto(&d, std::slice::from_ref(&existing)).unwrap();
        assert_eq!(
            edited.id, existing.id,
            "an explicit id edits that row rather than minting a new one",
        );
        assert_eq!(edited.label, "Renamed");
    }

    #[test]
    fn only_copy_and_move_are_schedulable() {
        for bad in ["delete", "secure-delete", "", "COPY"] {
            let mut d = dto();
            d.verb = bad.to_string();
            assert_eq!(
                entry_from_dto(&d, &[]).unwrap_err(),
                "err-schedule-verb-invalid"
            );
        }
    }

    #[test]
    fn out_of_range_trigger_fields_are_refused() {
        let mut d = dto();
        d.hour = 24;
        assert_eq!(
            entry_from_dto(&d, &[]).unwrap_err(),
            "err-schedule-trigger-invalid"
        );

        let mut d = dto();
        d.minute = 60;
        assert_eq!(
            entry_from_dto(&d, &[]).unwrap_err(),
            "err-schedule-trigger-invalid"
        );

        let mut d = dto();
        d.trigger = "fortnightly".to_string();
        assert_eq!(
            entry_from_dto(&d, &[]).unwrap_err(),
            "err-schedule-trigger-invalid"
        );
    }

    #[test]
    fn a_traversal_laden_path_is_refused_by_the_ipc_gate() {
        let mut d = dto();
        d.destination = if cfg!(windows) {
            r"D:\backup\..\..\Windows".to_string()
        } else {
            "/backup/../../etc".to_string()
        };
        assert!(entry_from_dto(&d, &[]).is_err());
    }

    #[test]
    fn a_control_character_in_the_label_is_refused() {
        let mut d = dto();
        d.label = "Nightly\n[Service]\nExecStart=/bin/sh".to_string();
        assert_eq!(
            entry_from_dto(&d, &[]).unwrap_err(),
            "err-schedule-label-invalid"
        );
    }

    #[test]
    fn the_argv_always_names_this_binary_and_the_enqueue_cli() {
        let entry = entry_from_dto(&dto(), &[]).unwrap();
        let job = scheduled_job_for(&entry).expect("current_exe resolves under test");
        assert_eq!(
            job.program,
            std::env::current_exe().unwrap(),
            "a schedule may only ever run this app",
        );
        assert_eq!(job.args[0], "--enqueue");
        assert_eq!(job.args[1], "copy");
        assert!(job.args.contains(&"--files-from".to_string()));
        assert!(job.args.contains(&"--destination".to_string()));
        assert!(
            !job.args.contains(&"--relative-to".to_string()),
            "an empty relative-to must not emit a bare flag",
        );
    }

    #[test]
    fn relative_to_is_forwarded_when_set() {
        let mut d = dto();
        d.relative_to = if cfg!(windows) {
            r"C:\photos".to_string()
        } else {
            "/photos".to_string()
        };
        let entry = entry_from_dto(&d, &[]).unwrap();
        let job = scheduled_job_for(&entry).unwrap();
        assert!(job.args.contains(&"--relative-to".to_string()));
    }

    #[test]
    fn next_run_preview_converts_through_the_utc_offset() {
        let entry = entry_from_dto(&dto(), &[]).unwrap(); // daily 03:30
        // 1970-01-01 00:00 UTC, host two hours ahead → local 02:00, so
        // the next 03:30 local is 90 minutes away, i.e. 01:30 UTC.
        let next = next_run_for(&entry, 0, 2 * 3_600, |_| 2 * 3_600).expect("daily resolves");
        assert_eq!(next, 90 * 60);
    }

    #[test]
    fn next_run_preview_uses_the_offset_at_the_firing_not_today() {
        // A zone that springs forward by an hour at T+2h: UTC-5 before,
        // UTC-4 after. Modelled exactly as the OS database would report
        // it — offset as a function of the *instant*.
        const SPRING_FORWARD: i64 = 2 * 3_600;
        let zone = |t: i64| {
            if t < SPRING_FORWARD {
                -5 * 3_600
            } else {
                -4 * 3_600
            }
        };

        let entry = entry_from_dto(&dto(), &[]).unwrap(); // daily 03:30
        let now = 0;
        let next = next_run_for(&entry, now, zone(now), zone).expect("daily resolves");

        // The job must fire when the local wall clock reads 03:30, and
        // by then the zone is UTC-4 — so the instant is 07:30 UTC, not
        // the 08:30 that today's UTC-5 offset would have produced.
        assert_eq!(
            next,
            7 * 3_600 + 30 * 60,
            "the preview must use the offset in effect at the firing",
        );
        // And it must genuinely land on 03:30 local when re-read
        // through the zone — the property the old code violated.
        assert_eq!(
            next + zone(next),
            3 * 3_600 + 30 * 60,
            "round-tripping the answer through the zone must give 03:30",
        );
    }

    #[test]
    fn an_unknown_trigger_yields_no_preview_rather_than_a_wrong_one() {
        let entry = ScheduleEntry {
            trigger: "eclipse".to_string(),
            ..ScheduleEntry::default()
        };
        assert_eq!(next_run_for(&entry, 0, 0, |_| 0), None);
    }

    #[test]
    fn decorate_reports_os_truth_not_the_settings_row() {
        let entries = vec![
            ScheduleEntry {
                id: "present".to_string(),
                trigger: "daily".to_string(),
                hour: 3,
                ..ScheduleEntry::default()
            },
            ScheduleEntry {
                id: "vanished".to_string(),
                trigger: "daily".to_string(),
                hour: 3,
                ..ScheduleEntry::default()
            },
        ];
        let dtos = decorate(&entries, 0, 0, |_| 0, |id| id == "present");
        assert!(dtos[0].installed);
        assert!(
            !dtos[1].installed,
            "a schedule removed behind the app's back must not read as live",
        );
        assert!(dtos.iter().all(|d| d.next_run_unix_secs.is_some()));
    }
}
