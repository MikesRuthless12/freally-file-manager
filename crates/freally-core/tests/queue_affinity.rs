//! FFM-M18 / FFM-M19 — queue-affinity overrides, per-job priority,
//! and moving a job between queues.
//!
//! Same `FakeProbe` shape as `queue_registry.rs`: fake `/drive/A` and
//! `/drive/B` paths map to deterministic drive ids so routing can be
//! observed without touching the host filesystem.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use freally_core::{
    AffinityGroup, JobId, JobKind, JobState, QueueMergeError, QueueRegistry, VolumeProbe,
};

#[derive(Debug)]
struct FakeProbe;

impl FakeProbe {
    fn drive_for(path: &Path) -> Option<u64> {
        let s = path.to_string_lossy();
        if s.contains("/drive/A") {
            Some(0xAAAA_AAAA)
        } else if s.contains("/drive/B") {
            Some(0xBBBB_BBBB)
        } else {
            None
        }
    }
}

impl VolumeProbe for FakeProbe {
    fn volume_id(&self, path: &Path) -> Option<u64> {
        FakeProbe::drive_for(path)
    }

    fn drive_label(&self, path: &Path) -> Option<String> {
        FakeProbe::drive_for(path).map(|id| format!("0x{id:08X}"))
    }
}

fn drive_a(name: &str) -> PathBuf {
    PathBuf::from(format!("/drive/A/{name}"))
}

fn drive_b(name: &str) -> PathBuf {
    PathBuf::from(format!("/drive/B/{name}"))
}

fn registry() -> QueueRegistry {
    QueueRegistry::new().with_probe(Arc::new(FakeProbe))
}

fn group(name: &str, prefixes: &[&str], workers: Option<u32>) -> AffinityGroup {
    AffinityGroup {
        name: name.to_string(),
        prefixes: prefixes.iter().map(PathBuf::from).collect(),
        workers,
    }
}

// ---------------------------------------------------------------------
// FFM-M18 — queue-affinity overrides
// ---------------------------------------------------------------------

#[test]
fn no_affinity_groups_leaves_probe_routing_untouched() {
    let reg = registry();
    reg.set_affinity_groups(Vec::new());

    let (qa, _, _) = reg.route(JobKind::Copy, drive_a("s"), Some(drive_a("d")));
    let (qb, _, _) = reg.route(JobKind::Copy, drive_b("s"), Some(drive_b("d")));
    assert_ne!(qa, qb, "an empty override list must not change bucketing");
}

#[test]
fn one_group_over_two_drives_force_merges_their_queues() {
    // The repo's own benchmark case: T: is a VHDX backed by C:, so the
    // probe reports two drives that are really one spindle.
    let reg = registry();
    reg.set_affinity_groups(vec![group("One spindle", &["/drive/A", "/drive/B"], None)]);

    let (qa, _, _) = reg.route(JobKind::Copy, drive_a("s"), Some(drive_a("d")));
    let (qb, _, _) = reg.route(JobKind::Copy, drive_b("s"), Some(drive_b("d")));

    assert_eq!(qa, qb, "both drives must share the group's queue");
    assert_eq!(reg.len(), 1);
    assert_eq!(
        reg.get(qa).expect("group queue").name(),
        "One spindle",
        "the queue takes the group's own name, verbatim",
    );
}

#[test]
fn separate_groups_force_split_one_physical_drive() {
    // Both paths probe to drive A, but the user knows they are backed
    // by independent hardware behind a controller.
    let reg = registry();
    reg.set_affinity_groups(vec![
        group("Shelf 1", &["/drive/A/one"], None),
        group("Shelf 2", &["/drive/A/two"], None),
    ]);

    let (q1, _, _) = reg.route(
        JobKind::Copy,
        PathBuf::from("/drive/A/one/s"),
        Some(PathBuf::from("/drive/A/one/d/x")),
    );
    let (q2, _, _) = reg.route(
        JobKind::Copy,
        PathBuf::from("/drive/A/two/s"),
        Some(PathBuf::from("/drive/A/two/d/x")),
    );

    assert_ne!(q1, q2, "distinct groups must not share a queue");
    assert_eq!(reg.len(), 2);
}

/// FFM-M18 §3.2 — the user types `d:\media`; the shell payload
/// delivers `D:\Media\2026`. On Windows and macOS those name the same
/// folder, so the override must engage. It used to fall through to the
/// volume probe and global concurrency, so the group displayed exactly
/// as configured while doing nothing at all.
#[test]
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn affinity_prefixes_match_case_insensitively_where_the_os_does() {
    let reg = registry();
    reg.set_affinity_groups(vec![group("One spindle", &["/DRIVE/a"], Some(1))]);

    let (q, _, _) = reg.route(JobKind::Copy, drive_a("s"), Some(drive_a("d")));
    assert_eq!(
        reg.get(q).expect("group queue").name(),
        "One spindle",
        "a case difference must not silently disable the override",
    );
}

/// ...and must **not** on Linux, where two names differing only in case
/// are genuinely two different directories.
#[test]
#[cfg(target_os = "linux")]
fn affinity_prefixes_stay_case_sensitive_on_linux() {
    let reg = registry();
    reg.set_affinity_groups(vec![group("One spindle", &["/DRIVE/a"], Some(1))]);

    let (q, _, _) = reg.route(JobKind::Copy, drive_a("s"), Some(drive_a("d")));
    assert_ne!(
        reg.get(q).expect("queue").name(),
        "One spindle",
        "/DRIVE/a and /drive/A are different directories on Linux",
    );
}

#[test]
fn the_longest_matching_prefix_wins() {
    let reg = registry();
    reg.set_affinity_groups(vec![
        group("Whole drive", &["/drive/A"], Some(1)),
        group("Fast scratch", &["/drive/A/scratch"], Some(8)),
    ]);

    let matched = reg
        .affinity_match(Path::new("/drive/A/scratch/deep"))
        .expect("nested path matches");
    assert_eq!(matched.1, "Fast scratch");
    assert_eq!(matched.2, Some(8));

    let parent = reg
        .affinity_match(Path::new("/drive/A/other"))
        .expect("parent path still matches the broader group");
    assert_eq!(parent.1, "Whole drive");
    assert_eq!(parent.2, Some(1));
}

#[test]
fn a_partial_component_is_not_a_prefix_match() {
    // `/drive/AB` must not be captured by a `/drive/A` group — a
    // string-prefix test would wrongly claim it.
    let reg = registry();
    reg.set_affinity_groups(vec![group("Drive A", &["/drive/A"], None)]);
    assert!(reg.affinity_match(Path::new("/drive/AB/x")).is_none());
}

#[test]
fn affinity_buckets_cannot_collide_with_probed_volume_ids() {
    let reg = registry();
    reg.set_affinity_groups(vec![group("G", &["/drive/A"], None)]);
    let (bucket, _, _) = reg.affinity_match(Path::new("/drive/A")).expect("matches");
    assert!(
        bucket & (1 << 63) != 0,
        "bucket ids must carry the high-bit tag that real volume ids never set",
    );
}

#[test]
fn empty_and_duplicate_groups_are_dropped() {
    let reg = registry();
    reg.set_affinity_groups(vec![
        group("", &["/drive/A"], None),
        group("No paths", &[], None),
        group("Dupe", &["/drive/A"], Some(2)),
        group("Dupe", &["/drive/B"], Some(9)),
    ]);
    let kept = reg.affinity_groups();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].name, "Dupe");
    assert_eq!(kept[0].workers, Some(2), "the first duplicate wins");
    assert!(
        reg.affinity_match(Path::new("/drive/B")).is_none(),
        "the dropped duplicate must not claim its prefixes",
    );
}

#[test]
fn bucket_ids_survive_reordering_the_group_list() {
    let reg = registry();
    reg.set_affinity_groups(vec![
        group("First", &["/drive/A"], None),
        group("Second", &["/drive/B"], None),
    ]);
    let before = reg.affinity_match(Path::new("/drive/B")).expect("match").0;
    reg.set_affinity_groups(vec![
        group("Second", &["/drive/B"], None),
        group("First", &["/drive/A"], None),
    ]);
    let after = reg.affinity_match(Path::new("/drive/B")).expect("match").0;
    assert_eq!(before, after, "reordering must not re-bucket a queue");
}

// ---------------------------------------------------------------------
// FFM-M19 — per-job priority and queue move
// ---------------------------------------------------------------------

#[test]
fn boost_pauses_only_running_siblings_and_restores_exactly_those() {
    let reg = registry();
    let (qid, a, _) = reg.route(JobKind::Copy, drive_a("s0"), Some(drive_a("d")));
    let (_, b, _) = reg.route(JobKind::Copy, drive_a("s1"), Some(drive_a("d")));
    let (_, c, _) = reg.route(JobKind::Copy, drive_a("s2"), Some(drive_a("d")));
    let queue = reg.get(qid).expect("queue");
    queue.start(a);
    queue.start(b);
    // `c` stays Pending, and the user paused it by hand beforehand.
    queue.pause_job(c);

    let paused = queue.boost_job(a);
    assert_eq!(paused, vec![b], "only running siblings are paused");
    assert_eq!(queue.get(b).expect("b").state, JobState::Paused);
    assert_eq!(queue.get(a).expect("a").state, JobState::Running);

    queue.clear_boost(&paused);
    assert_eq!(queue.get(b).expect("b").state, JobState::Running);
    assert_eq!(
        queue.get(c).expect("c").state,
        JobState::Paused,
        "a job the user paused before the boost must stay paused",
    );
}

#[test]
fn moving_a_pending_job_keeps_its_id_and_lands_in_the_target_queue() {
    let reg = registry();
    let (qa, job, _) = reg.route(JobKind::Copy, drive_a("s"), Some(drive_a("d")));
    let (qb, _, _) = reg.route(JobKind::Copy, drive_b("s"), Some(drive_b("d")));

    reg.move_job_to_queue(job, qb).expect("move succeeds");

    assert!(reg.get(qa).expect("A").get(job).is_none());
    let moved = reg.get(qb).expect("B").get(job).expect("job moved");
    assert_eq!(moved.id, job, "the job keeps its id across the move");
}

#[test]
fn a_running_job_refuses_to_change_queue() {
    let reg = registry();
    let (qa, job, _) = reg.route(JobKind::Copy, drive_a("s"), Some(drive_a("d")));
    let (qb, _, _) = reg.route(JobKind::Copy, drive_b("s"), Some(drive_b("d")));
    reg.get(qa).expect("A").start(job);

    assert!(
        reg.move_job_to_queue(job, qb).is_err(),
        "re-parenting a running copy would change its concurrency mid-flight",
    );
    assert!(reg.get(qa).expect("A").get(job).is_some());
}

#[test]
fn a_paused_job_refuses_to_change_queue() {
    // A paused job is still owned by a live runner task holding the
    // *source* queue's handle. Moving it removed the entry there, and
    // when the copy eventually finished the runner's finish_ok /
    // finish_fail found no matching entry and returned early — the job
    // then sat in the destination queue in a non-terminal state that
    // nothing would ever complete.
    let reg = registry();
    let (qa, job, _) = reg.route(JobKind::Copy, drive_a("s"), Some(drive_a("d")));
    let (qb, _, _) = reg.route(JobKind::Copy, drive_b("s"), Some(drive_b("d")));
    let a = reg.get(qa).expect("A");
    a.start(job);
    a.pause_job(job);

    assert!(
        reg.move_job_to_queue(job, qb).is_err(),
        "a paused job is still runner-owned; moving it strands it forever",
    );
    assert!(
        reg.get(qa).expect("A").get(job).is_some(),
        "it must stay in the queue its runner still holds",
    );
}

#[test]
fn moving_a_job_into_its_own_queue_is_a_noop() {
    let reg = registry();
    let (qa, job, _) = reg.route(JobKind::Copy, drive_a("s"), Some(drive_a("d")));
    reg.move_job_to_queue(job, qa).expect("no-op succeeds");
    assert!(reg.get(qa).expect("A").get(job).is_some());
}

#[test]
fn moving_an_unknown_job_errors_rather_than_silently_doing_nothing() {
    let reg = registry();
    let (_, _, _) = reg.route(JobKind::Copy, drive_a("s"), Some(drive_a("d")));
    let (qb, _, _) = reg.route(JobKind::Copy, drive_b("s"), Some(drive_b("d")));
    // `UnknownJob`, not `UnknownSrc`: no queue holds this job, so there
    // is no source queue id to name. This used to assert `UnknownSrc`,
    // which the code satisfied by reporting the *destination* queue —
    // the one it had just found without trouble.
    let err = reg
        .move_job_to_queue(JobId::from_u64(9_999), qb)
        .expect_err("moving a job nothing holds must be an error");
    match err {
        QueueMergeError::UnknownJob(id) => {
            assert_eq!(id, JobId::from_u64(9_999), "must name the job asked for");
        }
        other => panic!("expected UnknownJob, got {other:?}"),
    }
}
