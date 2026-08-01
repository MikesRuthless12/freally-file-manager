//! Build 3 (FFM-M17..M24) — IPC roundtrip tests.
//!
//! Same shape as `queue_commands.rs`: the `#[tauri::command]` shells
//! need a live runtime, so these drive the `*_impl(&AppState, …)`
//! helpers that hold the logic.
//!
//! What is deliberately *not* here: anything that installs a real OS
//! schedule or login item. `schedule_save_impl` shells out to
//! `schtasks` / `launchctl` / `systemctl` and `autostart_set_impl`
//! writes an HKCU value — a test suite that did either would leave
//! artifacts on the developer's machine and on CI runners. The
//! rendering and validation those paths depend on is unit-tested in
//! `freally-platform::scheduler` / `::autostart`; what is covered here
//! is everything up to the OS boundary.

use std::path::PathBuf;

use freally_settings::{FavoriteEntry, ProfileStore, ScheduleEntry, Settings};
use freally_ui_lib::favorites_commands::{
    FavoriteDto, favorites_list_impl, favorites_record_pair_impl, favorites_remove_impl,
    favorites_save_impl,
};
use freally_ui_lib::queue_commands::{
    AffinityGroupDto, apply_persisted_affinity, to_core_groups, validate_affinity_groups,
};
use freally_ui_lib::schedule_commands::{ScheduleDto, decorate, entry_from_dto, next_run_for};
use freally_ui_lib::state::AppState;

fn state() -> AppState {
    AppState::new_with(
        None,
        Settings::default(),
        // Empty path → the impls skip persistence, so tests never write
        // into the developer's real OS config directory.
        PathBuf::new(),
        ProfileStore::new(PathBuf::new()),
    )
}

fn abs(rel: &str) -> String {
    if cfg!(windows) {
        format!(r"C:\{}", rel.replace('/', r"\"))
    } else {
        format!("/{rel}")
    }
}

// ---------------------------------------------------------------------
// FFM-M17 — schedules
// ---------------------------------------------------------------------

fn schedule_dto() -> ScheduleDto {
    ScheduleDto {
        id: "nightly-photos".to_string(),
        label: "Nightly photos".to_string(),
        verb: "copy".to_string(),
        files_from: abs("lists/photos.txt"),
        destination: abs("backup"),
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
fn a_schedule_dto_normalises_and_keeps_its_fields() {
    let entry = entry_from_dto(&schedule_dto(), &[]).expect("valid dto");
    assert_eq!(entry.id, "nightly-photos");
    assert_eq!(entry.verb, "copy");
    assert_eq!(entry.trigger, "daily");
    assert_eq!(entry.hour, 3);
    assert_eq!(entry.minute, 30);
    assert!(entry.run_when_available);
}

#[test]
fn the_ipc_gate_rejects_every_hostile_schedule_field() {
    /// One "make this field hostile" mutation, named for the assert.
    type Mutation = (&'static str, Box<dyn Fn(&mut ScheduleDto)>);

    let cases: Vec<Mutation> = vec![
        (
            "id",
            Box::new(|d: &mut ScheduleDto| d.id = "../evil".into()),
        ),
        (
            "label",
            Box::new(|d: &mut ScheduleDto| d.label = "a\nExecStart=/bin/sh".into()),
        ),
        (
            "verb",
            Box::new(|d: &mut ScheduleDto| d.verb = "secure-delete".into()),
        ),
        (
            "trigger",
            Box::new(|d: &mut ScheduleDto| d.trigger = "eclipse".into()),
        ),
        ("hour", Box::new(|d: &mut ScheduleDto| d.hour = 24)),
        ("minute", Box::new(|d: &mut ScheduleDto| d.minute = 60)),
        ("weekday", Box::new(|d: &mut ScheduleDto| d.weekday = 7)),
    ];
    for (name, mutate) in cases {
        let mut dto = schedule_dto();
        mutate(&mut dto);
        assert!(
            entry_from_dto(&dto, &[]).is_err(),
            "a hostile `{name}` must be refused",
        );
    }
}

#[test]
fn the_next_run_preview_is_offset_aware() {
    let entry = entry_from_dto(&schedule_dto(), &[]).unwrap(); // daily 03:30
    // Epoch, host two hours ahead of UTC → local 02:00, so the next
    // 03:30 local is 90 minutes out.
    assert_eq!(next_run_for(&entry, 0, 2 * 3_600), Some(90 * 60));
    // Same instant, host on UTC → 03:30 is 210 minutes out.
    assert_eq!(next_run_for(&entry, 0, 0), Some(210 * 60));
}

#[test]
fn decorate_reports_the_os_view_not_the_stored_row() {
    let entries = vec![
        ScheduleEntry {
            id: "present".into(),
            trigger: "daily".into(),
            hour: 3,
            ..ScheduleEntry::default()
        },
        ScheduleEntry {
            id: "vanished".into(),
            trigger: "daily".into(),
            hour: 3,
            ..ScheduleEntry::default()
        },
    ];
    let dtos = decorate(&entries, 0, 0, |id| id == "present");
    assert!(dtos[0].installed);
    assert!(
        !dtos[1].installed,
        "a schedule deleted behind the app's back must not read as live",
    );
}

// ---------------------------------------------------------------------
// FFM-M18 — queue affinity
// ---------------------------------------------------------------------

fn group(name: &str, prefixes: &[&str], workers: u32) -> AffinityGroupDto {
    AffinityGroupDto {
        name: name.to_string(),
        prefixes: prefixes.iter().map(|p| (*p).to_string()).collect(),
        workers,
    }
}

#[test]
fn a_valid_affinity_group_survives_validation() {
    let src = abs("drive-a");
    let groups = validate_affinity_groups(&[group("One spindle", &[&src], 1)]).expect("valid");
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].name, "One spindle");
    assert_eq!(groups[0].workers, 1);
}

#[test]
fn affinity_validation_refuses_the_shapes_that_would_break_routing() {
    let src = abs("drive-a");
    // Duplicate names would silently share a bucket, merging two
    // groups the user meant to keep apart.
    assert!(
        validate_affinity_groups(&[
            group("Same", &[&src], 0),
            group("Same", &[&abs("drive-b")], 0),
        ])
        .is_err(),
        "duplicate group names must be refused",
    );
    assert!(
        validate_affinity_groups(&[group("", &[&src], 0)]).is_err(),
        "an empty name must be refused",
    );
    assert!(
        validate_affinity_groups(&[group("No paths", &[], 0)]).is_err(),
        "a group with no folders must be refused",
    );
    assert!(
        validate_affinity_groups(&[group("Too many", &[&src], 17)]).is_err(),
        "a worker count past the engine's ceiling must be refused",
    );
    assert!(
        validate_affinity_groups(&[group("Traversal", &[&abs("drive-a/../../etc")], 0)]).is_err(),
        "a traversal-laden prefix must not steer routing",
    );
}

#[test]
fn zero_workers_maps_to_inherit_not_to_zero_threads() {
    let src = abs("drive-a");
    let validated = validate_affinity_groups(&[group("Auto", &[&src], 0)]).unwrap();
    let core = to_core_groups(&validated);
    assert_eq!(
        core[0].workers, None,
        "`0` in settings means inherit the global count, not spawn none",
    );

    let validated = validate_affinity_groups(&[group("Hdd", &[&src], 1)]).unwrap();
    assert_eq!(to_core_groups(&validated)[0].workers, Some(1));
}

#[test]
fn persisted_affinity_groups_reach_the_live_registry_at_startup() {
    let s = state();
    {
        let mut settings = s.settings.write().unwrap();
        settings.queue.affinity_groups = validate_affinity_groups(&[group(
            "One spindle",
            &[&abs("drive-a"), &abs("drive-b")],
            2,
        )])
        .unwrap();
    }
    // Before the startup hook runs, the registry knows nothing.
    assert!(s.queues.affinity_groups().is_empty());

    apply_persisted_affinity(&s);

    let live = s.queues.affinity_groups();
    assert_eq!(live.len(), 1, "the restart hook must republish overrides");
    assert_eq!(live[0].name, "One spindle");
    assert_eq!(live[0].workers, Some(2));
}

// ---------------------------------------------------------------------
// FFM-M20 — favorites
// ---------------------------------------------------------------------

fn favorite_dto(kind: &str) -> FavoriteDto {
    FavoriteDto {
        id: String::new(),
        label: "Photos 2026".to_string(),
        kind: kind.to_string(),
        path: abs("photos"),
        destination: abs("backup"),
        hotkey: String::new(),
    }
}

#[test]
fn saving_a_favorite_round_trips_through_settings() {
    let s = state();
    let list = favorites_save_impl(&s, &favorite_dto("pair")).expect("saved");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].label, "Photos 2026");
    assert_eq!(list[0].kind, "pair");
    assert!(!list[0].id.is_empty(), "the backend must mint an id");

    assert_eq!(favorites_list_impl(&s).len(), 1);
}

#[test]
fn saving_with_an_existing_id_updates_in_place() {
    let s = state();
    let created = favorites_save_impl(&s, &favorite_dto("destination")).unwrap();
    let id = created[0].id.clone();

    let mut edit = favorite_dto("destination");
    edit.id = id.clone();
    edit.label = "Photos (renamed)".to_string();
    let after = favorites_save_impl(&s, &edit).unwrap();

    assert_eq!(after.len(), 1, "an edit must not append a second row");
    assert_eq!(after[0].id, id);
    assert_eq!(after[0].label, "Photos (renamed)");
}

#[test]
fn removing_a_favorite_is_idempotent() {
    let s = state();
    let created = favorites_save_impl(&s, &favorite_dto("source")).unwrap();
    let id = created[0].id.clone();

    assert!(favorites_remove_impl(&s, &id).unwrap().is_empty());
    assert!(
        favorites_remove_impl(&s, &id).unwrap().is_empty(),
        "removing a favorite twice must not error",
    );
}

#[test]
fn a_favorite_with_a_hostile_path_is_refused() {
    let s = state();
    let mut dto = favorite_dto("source");
    dto.path = abs("photos/../../secrets");
    assert!(favorites_save_impl(&s, &dto).is_err());
    assert!(
        favorites_list_impl(&s).is_empty(),
        "a rejected save must leave nothing behind",
    );
}

#[test]
fn recording_a_pair_is_mru_ordered_and_deduplicated() {
    let s = state();
    let a = abs("src-a");
    let b = abs("src-b");
    let dst = abs("dst");

    favorites_record_pair_impl(&s, &a, &dst, 1).unwrap();
    let ring = favorites_record_pair_impl(&s, &b, &dst, 2).unwrap();
    assert_eq!(ring.len(), 2);
    assert_eq!(ring[0].source, b, "newest first");

    let ring = favorites_record_pair_impl(&s, &a, &dst, 3).unwrap();
    assert_eq!(ring.len(), 2, "a repeat must move, not append");
    assert_eq!(ring[0].source, a);
    assert_eq!(ring[0].used_at_ms, 3, "the timestamp must refresh");
}

#[test]
fn recording_a_pair_gates_both_sides_through_the_ipc_validator() {
    let s = state();
    let good = abs("src");
    assert!(favorites_record_pair_impl(&s, &good, &abs("dst/../../etc"), 1).is_err());
    assert!(favorites_record_pair_impl(&s, &abs("src/../../etc"), &good, 1).is_err());
}

// ---------------------------------------------------------------------
// Cross-cutting — the wholesale-replace carry-over
// ---------------------------------------------------------------------

#[test]
fn a_settings_modal_save_cannot_discard_build_3_state() {
    // The Settings DTO models neither schedules nor favorites nor
    // affinity groups, and `into_settings` rebuilds from Default. Every
    // one of them would be wiped by an ordinary "Save" in the modal
    // without `carry_backend_owned_from`.
    let mut prev = Settings::default();
    prev.schedules.entries.push(ScheduleEntry {
        id: "nightly".into(),
        label: "Nightly".into(),
        ..ScheduleEntry::default()
    });
    prev.favorites.entries.push(FavoriteEntry {
        id: "f1".into(),
        label: "Photos".into(),
        ..FavoriteEntry::default()
    });
    prev.queue.affinity_groups =
        validate_affinity_groups(&[group("One spindle", &[&abs("drive-a")], 1)]).unwrap();

    let mut next = Settings::default();
    next.general.language = "de".into();
    next.carry_backend_owned_from(&prev);

    assert_eq!(next.general.language, "de", "the modal's edit still lands");
    assert_eq!(next.schedules.entries.len(), 1);
    assert_eq!(next.favorites.entries.len(), 1);
    assert_eq!(next.queue.affinity_groups.len(), 1);
}
