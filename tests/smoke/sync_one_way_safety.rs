//! A one-way sync must never write to, or rename a file on, its own
//! authoritative side — and an ordinary filename must not wedge a pair.
//!
//! Three defects this locks out, all found by review on 2026-08-19:
//!
//! 1. The "right changed since baseline" arm copied R→L for every mode
//!    except Contribute, so `MirrorLeftToRight` propagated the replica's
//!    edit back over its own source.
//! 2. Conflicts were decided purely by mtime, including in one-way
//!    modes. On a first mirror run against a pre-populated destination
//!    there is no baseline, so every differing file is an add/add
//!    conflict — and copy tools stamp destination mtimes at copy time,
//!    so the destination usually "won", the source got renamed to
//!    `name.sync-conflict-…`, and the replica overwrote it.
//! 3. `join_relpath` rejected any relpath *containing* `..` (and any
//!    `:`), not just a `..` segment. `archive..2024.zip` therefore
//!    failed the whole pass — and every action sorting after it — on
//!    every run, forever.

use freally_sync::VersionVector;
use freally_sync::db::FileRecord;
use freally_sync::engine::decide_action;
use freally_sync::types::{Direction, FileMeta, SideState, SyncAction, SyncMode};

fn hash(seed: u8) -> [u8; 32] {
    [seed; 32]
}

fn side(relpath: &str, seed: Option<u8>, mtime_ms: i64) -> SideState {
    SideState {
        relpath: relpath.to_string(),
        meta: seed.map(|s| FileMeta {
            mtime_ms,
            size: 8,
            blake3: hash(s),
        }),
    }
}

fn baseline(seed: u8, mtime_ms: i64) -> FileRecord {
    FileRecord {
        vv: VersionVector::default(),
        mtime_ms,
        size: 8,
        blake3: hash(seed),
    }
}

#[test]
fn mirror_left_to_right_restores_a_drifted_replica_instead_of_copying_it_back() {
    // Baseline exists; left still matches it, right was edited. The
    // right is the replica under this mode, so it must be restored FROM
    // the left — never copied back onto it.
    let action = decide_action(
        "a.txt",
        &side("a.txt", Some(1), 1_000),
        &side("a.txt", Some(2), 9_999),
        Some(&baseline(1, 1_000)),
        SyncMode::MirrorLeftToRight,
    );
    match action {
        SyncAction::Copy { direction, .. } => assert_eq!(
            direction,
            Direction::LeftToRight,
            "a mirror must restore its replica, not copy the replica's edit back",
        ),
        other => panic!("expected a restoring Copy, got {other:?}"),
    }
}

#[test]
fn mirror_right_to_left_restores_its_own_replica_too() {
    let action = decide_action(
        "a.txt",
        &side("a.txt", Some(2), 9_999), // left (the replica here) edited
        &side("a.txt", Some(1), 1_000),
        Some(&baseline(1, 1_000)),
        SyncMode::MirrorRightToLeft,
    );
    match action {
        SyncAction::Copy { direction, .. } => {
            assert_eq!(direction, Direction::RightToLeft)
        }
        other => panic!("expected a restoring Copy, got {other:?}"),
    }
}

#[test]
fn two_way_still_propagates_a_one_sided_edit() {
    // Guard against over-rotating: two-way must keep propagating.
    let action = decide_action(
        "a.txt",
        &side("a.txt", Some(1), 1_000),
        &side("a.txt", Some(2), 9_999),
        Some(&baseline(1, 1_000)),
        SyncMode::TwoWay,
    );
    match action {
        SyncAction::Copy { direction, .. } => {
            assert_eq!(direction, Direction::RightToLeft)
        }
        other => panic!("two-way must propagate the edit, got {other:?}"),
    }
}

#[test]
fn a_mirror_conflict_is_won_by_the_authoritative_side_not_the_newer_mtime() {
    // No baseline + differing content = add/add. The right is far
    // newer, exactly the shape a pre-populated destination produces.
    let action = decide_action(
        "a.txt",
        &side("a.txt", Some(1), 1),
        &side("a.txt", Some(2), 9_999_999),
        None,
        SyncMode::MirrorLeftToRight,
    );
    match action {
        SyncAction::KeepConflict { winner, .. } => assert_eq!(
            winner,
            Direction::LeftToRight,
            "the authoritative left must win regardless of mtime",
        ),
        other => panic!("expected KeepConflict, got {other:?}"),
    }
}

#[test]
fn two_way_conflicts_still_go_to_the_newer_side() {
    let action = decide_action(
        "a.txt",
        &side("a.txt", Some(1), 1),
        &side("a.txt", Some(2), 9_999_999),
        None,
        SyncMode::TwoWay,
    );
    match action {
        SyncAction::KeepConflict { winner, .. } => assert_eq!(
            winner,
            Direction::RightToLeft,
            "two-way has no authoritative side, so mtime decides",
        ),
        other => panic!("expected KeepConflict, got {other:?}"),
    }
}
