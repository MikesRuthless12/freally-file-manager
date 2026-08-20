//! Sync engine — the `sync()` entry point, the decision matrix, and
//! the action executor.
//!
//! The engine is strictly serial per pair: one walk pass, one
//! decision pass, one execution pass. A single pair stays within one
//! tokio task so there's no cross-thread coordination to reason
//! about. Parallelism across pairs is the runner's concern.
//!
//! # Winner selection
//!
//! When both sides diverged concurrently from a common ancestor, the
//! engine picks a `winner` by mtime (later wins; ties go to `Left`
//! for determinism). The loser's current content is preserved on its
//! own side as `name.sync-conflict-YYYYMMDD-HHMMSS-<host>.ext`, and
//! the winner's content is propagated to the loser's canonical path.
//! Both sides are still surfaced to the UI so the user can override
//! the automatic choice.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{TimeZone, Utc};
use tokio::sync::mpsc;

use freally_core::{CopyControl, CopyEvent, CopyOptions, copy_file};

use crate::clock::resolve_concurrent;
use crate::control::SyncControl;
use crate::db::{FileRecord, HistoryEntry, HistoryKind, SyncDb};
use crate::error::{Result, SyncError};
use crate::types::{
    Conflict, ConflictKind, CopyHookFactory, Direction, FileMeta, SideState, SyncAction, SyncEvent,
    SyncMode, SyncOptions, SyncPair, SyncReport,
};
use crate::walker::{fold_relpath, is_case_insensitive, join_relpath, scan_side};

/// The one public entry point.
///
/// Runs one reconciliation round of `pair` under `mode`. Emits
/// progress events on `events`; returns a [`SyncReport`] summary on
/// completion.
///
/// The pair's DB is opened (or created) at `pair.db_path`. The DB
/// file itself is excluded from the walk, so placing it under the
/// left root by default is safe.
pub async fn sync(
    pair: &SyncPair,
    mode: SyncMode,
    opts: SyncOptions,
    ctrl: SyncControl,
    events: mpsc::Sender<SyncEvent>,
) -> Result<SyncReport> {
    let start = SystemTime::now();
    let db = SyncDb::open(&pair.db_path)?;

    ctrl.wait_while_paused().await?;
    let _ = events
        .send(SyncEvent::WalkStarted {
            side: Direction::LeftToRight,
        })
        .await;
    // Files the walk must never treat as user data.
    //
    // This was the hardcoded default name alone. Nothing stops a pair
    // from being configured with a `db_path` *inside* one of its own
    // roots, and when it is, the baseline was walked like any other
    // file: propagated to the other side, and — once it changed under
    // the walk, which it does constantly — deleted there. On Linux,
    // where nothing holds an exclusive lock on it, that is a route to
    // destroying the baseline mid-run. Derive the list from the path
    // actually in use, sidecars included.
    let mut skip: Vec<String> = vec![".freally-sync.db".to_string()];
    if let Some(name) = pair.db_path.file_name().and_then(|n| n.to_str()) {
        for suffix in ["", "-wal", "-shm", ".lock"] {
            let candidate = format!("{name}{suffix}");
            if !skip.contains(&candidate) {
                skip.push(candidate);
            }
        }
    }
    let left = {
        let root = pair.left.clone();
        let skip_vec = skip.clone();
        tokio::task::spawn_blocking(move || {
            let refs: Vec<&str> = skip_vec.iter().map(String::as_str).collect();
            scan_side(&root, &refs)
        })
        .await
        .map_err(|e| SyncError::Io {
            path: pair.left.clone(),
            source: std::io::Error::other(e.to_string()),
        })??
    };
    let _ = events
        .send(SyncEvent::WalkCompleted {
            side: Direction::LeftToRight,
            files_total: left.len() as u64,
        })
        .await;

    ctrl.wait_while_paused().await?;
    let _ = events
        .send(SyncEvent::WalkStarted {
            side: Direction::RightToLeft,
        })
        .await;
    let right = {
        let root = pair.right.clone();
        let skip_vec = skip.clone();
        tokio::task::spawn_blocking(move || {
            let refs: Vec<&str> = skip_vec.iter().map(String::as_str).collect();
            scan_side(&root, &refs)
        })
        .await
        .map_err(|e| SyncError::Io {
            path: pair.right.clone(),
            source: std::io::Error::other(e.to_string()),
        })??
    };
    let _ = events
        .send(SyncEvent::WalkCompleted {
            side: Direction::RightToLeft,
            files_total: right.len() as u64,
        })
        .await;

    // Collect the universe of relpaths seen on either side + any
    // relpath with a live baseline row.
    let db_keys: Vec<String> = db.all_files()?.into_iter().map(|(k, _)| k).collect();

    // On a case-insensitive volume two spellings name one file, and the
    // two sides are walked independently, so nothing stops them
    // reporting different ones. Collapse them before the union or the
    // engine sees two files that are each "missing" from the other
    // side — see `unify_case_variants` for what that costs.
    let (left, right) = if is_case_insensitive(&pair.left) || is_case_insensitive(&pair.right) {
        unify_case_variants(left, right, &db_keys)
    } else {
        (left, right)
    };

    let mut relpaths: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    relpaths.extend(left.keys().cloned());
    relpaths.extend(right.keys().cloned());
    // A baseline row spelled differently from the live sides must not
    // enter the union as a third, phantom relpath — it would be planned
    // as a deletion of a file that is present under its other spelling.
    let seen_folds: std::collections::HashSet<String> =
        relpaths.iter().map(|r| fold_relpath(r)).collect();
    for k in db_keys {
        if seen_folds.contains(&fold_relpath(&k)) {
            continue;
        }
        relpaths.insert(k);
    }

    // Decide every action up-front, then execute. The two-pass split
    // makes dry-run trivial (return after decide) and lets the UI
    // render the planned list before any copy fires.
    let mut actions: Vec<SyncAction> = Vec::with_capacity(relpaths.len());
    let device_self = db.device_id();
    for relpath in &relpaths {
        ctrl.wait_while_paused().await?;
        let baseline = db.get_file(relpath)?;
        let l = left.get(relpath).cloned().unwrap_or(SideState {
            relpath: relpath.clone(),
            meta: None,
        });
        let r = right.get(relpath).cloned().unwrap_or(SideState {
            relpath: relpath.clone(),
            meta: None,
        });
        // The three-tree matrix is symmetric and has no notion of a
        // forbidden direction, so it is only consulted for `TwoWay`.
        // A one-way mode stays on Phase 25's engine, which applies
        // `direction_allowed` (and contribute's never-delete rule)
        // internally. Routing a mirror through the symmetric matrix
        // would push destination-side edits back to the source.
        let action = if opts.tri_tree && matches!(mode, SyncMode::TwoWay) {
            decide_action_tri(relpath, &l, &r, baseline.as_ref())
        } else {
            decide_action(relpath, &l, &r, baseline.as_ref(), mode)
        };
        let _ = events
            .send(SyncEvent::ActionPlanned {
                action: action.clone(),
            })
            .await;
        actions.push(action);
    }

    let mut report = SyncReport::default();
    let host_label = pair
        .host_label
        .clone()
        .unwrap_or_else(|| format!("device-{}", short_device(device_self)));

    if opts.dry_run {
        report.elapsed_ms = elapsed_ms(start);
        let _ = events
            .send(SyncEvent::Finished {
                report: report.clone(),
            })
            .await;
        return Ok(report);
    }

    // Execute. Copies and deletes run serially per pair; we could
    // parallelise across relpaths but the payoff is small for the
    // typical home-user pair and the serial layout keeps the
    // conflict-file naming deterministic (timestamp + seq).
    let copy_factory = opts.copy_hook.clone().unwrap_or_else(|| {
        Arc::new(DefaultCopyHookFactory::new(&opts)) as Arc<dyn CopyHookFactory>
    });

    let mut history_seq: u64 = 0;
    for action in &actions {
        // A cancel that lands while the run is *paused* wakes
        // `wait_while_paused` with `SyncError::Cancelled`. Propagating
        // that with `?` turned an ordinary user cancel into a hard
        // failure and discarded the report — including the count of
        // actions already applied, which the UI needs to tell the user
        // what actually happened before they hit cancel. Cancelling
        // while *running* already took the clean path just below; both
        // now end the same way.
        match ctrl.wait_while_paused().await {
            Ok(()) => {}
            Err(SyncError::Cancelled) => {
                report.cancelled = true;
                break;
            }
            Err(e) => return Err(e),
        }
        if ctrl.is_cancelled() {
            report.cancelled = true;
            break;
        }

        let _ = events
            .send(SyncEvent::ActionStarted {
                action: action.clone(),
            })
            .await;

        match action {
            SyncAction::Noop { relpath } => {
                // When both sides hold identical content, establish
                // or refresh the baseline so future edits are
                // detectable. Without this the second sync round
                // can't tell "nothing changed" from "never seen".
                let left_meta = left.get(relpath).and_then(|s| s.meta.clone());
                let right_meta = right.get(relpath).and_then(|s| s.meta.clone());
                let agreed_meta = match (&left_meta, &right_meta) {
                    (Some(l), Some(r)) if l.blake3 == r.blake3 => Some(l.clone()),
                    _ => None,
                };
                if let Some(meta) = agreed_meta {
                    let baseline = db.get_file(relpath)?;
                    let needs_update = match &baseline {
                        Some(b) => b.blake3 != meta.blake3 || b.size != meta.size,
                        None => true,
                    };
                    if needs_update {
                        let mut vv = baseline.map(|b| b.vv).unwrap_or_default();
                        vv.increment(device_self);
                        db.put_file(
                            relpath,
                            &FileRecord {
                                vv,
                                mtime_ms: meta.mtime_ms,
                                size: meta.size,
                                blake3: meta.blake3,
                            },
                        )?;
                    }
                } else if left_meta.is_none() && right_meta.is_none() {
                    // Both absent but baseline row existed —
                    // prune it so the state store stays compact.
                    if db.get_file(relpath)?.is_some() {
                        db.delete_file(relpath)?;
                    }
                }
            }
            SyncAction::Copy { relpath, direction } => {
                apply_copy(pair, relpath, *direction, copy_factory.as_ref()).await?;
                let side_state = match direction {
                    Direction::LeftToRight => left.get(relpath),
                    Direction::RightToLeft => right.get(relpath),
                }
                .expect("copy direction implies source present");
                let meta = side_state.meta.as_ref().expect("source has meta");
                // Bump vv on the *source* device (that's us on a
                // local-initiated sync). The DB records the merged
                // view so next round sees the propagated vv.
                let baseline = db.get_file(relpath)?;
                let mut vv = baseline.map(|b| b.vv).unwrap_or_default();
                vv.increment(device_self);
                let rec = FileRecord {
                    vv: vv.clone(),
                    mtime_ms: meta.mtime_ms,
                    size: meta.size,
                    blake3: meta.blake3,
                };
                db.put_file(relpath, &rec)?;
                match direction {
                    Direction::LeftToRight => report.applied_right += 1,
                    Direction::RightToLeft => report.applied_left += 1,
                }
                history_seq += 1;
                db.append_history(&HistoryEntry {
                    timestamp_ms: now_ms(),
                    seq: history_seq,
                    relpath: relpath.clone(),
                    kind: HistoryKind::Propagated,
                    direction: Some(*direction),
                    vv_before: None,
                    vv_after: Some(vv),
                })?;
            }
            SyncAction::Delete { relpath, direction } => {
                // Re-stat the source at apply time so a race between
                // the walk pass and now (source re-created with newer
                // bytes) doesn't propagate a stale delete. The
                // baseline is needed inside `apply_delete` to decide
                // whether the re-created content is "new" relative to
                // the last-synced state.
                let baseline = db.get_file(relpath)?;
                let outcome = apply_delete(pair, relpath, *direction, baseline.as_ref()).await?;
                match outcome {
                    // Nothing changed on disk — leave the baseline,
                    // the counters and the history untouched.
                    DeleteOutcome::Skipped => {}
                    DeleteOutcome::Deleted => {
                        db.delete_file(relpath)?;
                        match direction {
                            Direction::LeftToRight => report.deleted_right += 1,
                            Direction::RightToLeft => report.deleted_left += 1,
                        }
                        history_seq += 1;
                        db.append_history(&HistoryEntry {
                            timestamp_ms: now_ms(),
                            seq: history_seq,
                            relpath: relpath.clone(),
                            kind: HistoryKind::Deleted,
                            direction: Some(*direction),
                            vv_before: None,
                            vv_after: None,
                        })?;
                    }
                    DeleteOutcome::RaceAbortedAsConflict(conflict) => {
                        // The delete was suppressed; record the race
                        // as a delete-edit conflict so the user sees
                        // the relpath in the report and the next
                        // sync round can resolve it deterministically.
                        history_seq += 1;
                        db.append_history(&HistoryEntry {
                            timestamp_ms: now_ms(),
                            seq: history_seq,
                            relpath: relpath.clone(),
                            kind: HistoryKind::Conflict,
                            direction: Some(conflict.winner),
                            vv_before: None,
                            vv_after: None,
                        })?;
                        report.conflicts.push(conflict.clone());
                        let _ = events.send(SyncEvent::Conflict(conflict)).await;
                    }
                }
            }
            SyncAction::KeepConflict {
                relpath,
                winner,
                loser,
                kind,
            } => {
                let conflict = apply_conflict(
                    pair,
                    relpath,
                    *winner,
                    *loser,
                    *kind,
                    &host_label,
                    copy_factory.as_ref(),
                )
                .await?;
                // Baseline becomes a merged VV of both sides'
                // current vectors, bumped by self so future rounds
                // see it as descending both ancestors.
                let left_meta = left.get(relpath).and_then(|s| s.meta.clone());
                let right_meta = right.get(relpath).and_then(|s| s.meta.clone());
                let baseline = db.get_file(relpath)?;
                let mut vv_left = baseline.as_ref().map(|b| b.vv.clone()).unwrap_or_default();
                if left_meta.is_some() {
                    vv_left.increment(device_self);
                }
                let mut vv_right = baseline.as_ref().map(|b| b.vv.clone()).unwrap_or_default();
                if right_meta.is_some() {
                    vv_right.increment(device_self);
                }
                let merged = resolve_concurrent(&vv_left, &vv_right, device_self);
                let winner_meta = match winner {
                    Direction::LeftToRight => left_meta.clone(),
                    Direction::RightToLeft => right_meta.clone(),
                };
                if let Some(meta) = winner_meta {
                    db.put_file(
                        relpath,
                        &FileRecord {
                            vv: merged.clone(),
                            mtime_ms: meta.mtime_ms,
                            size: meta.size,
                            blake3: meta.blake3,
                        },
                    )?;
                }
                history_seq += 1;
                db.append_history(&HistoryEntry {
                    timestamp_ms: now_ms(),
                    seq: history_seq,
                    relpath: relpath.clone(),
                    kind: match kind {
                        ConflictKind::CorruptEqual => HistoryKind::Corrupt,
                        _ => HistoryKind::Conflict,
                    },
                    direction: Some(*winner),
                    vv_before: None,
                    vv_after: Some(merged),
                })?;
                report.conflicts.push(conflict.clone());
                let _ = events.send(SyncEvent::Conflict(conflict)).await;
            }
        }

        let _ = events
            .send(SyncEvent::ActionCompleted {
                action: action.clone(),
            })
            .await;
    }

    report.elapsed_ms = elapsed_ms(start);
    let _ = events
        .send(SyncEvent::Finished {
            report: report.clone(),
        })
        .await;
    Ok(report)
}

/// The conflict matrix, pure function for testability.
///
/// `baseline` is the last-seen-synced row; `left` + `right` are the
/// current walk observations (with `meta = None` meaning absent).
/// `mode` controls whether a one-way mirror should consider deletes.
/// Phase 52 — decide one path with the three-tree state machine.
///
/// Left maps to "local", right to "remote", and the Phase 25 baseline
/// record to the synced tree. The result is translated back into the
/// existing [`SyncAction`] vocabulary so the executor, the event
/// stream, and the conflict UI are all unchanged — only the decision
/// changes.
///
/// `mode` is not a parameter because the three-tree matrix is
/// symmetric — it has no concept of a forbidden direction. The caller
/// ([`sync`]) therefore only routes `SyncMode::TwoWay` here and leaves
/// every one-way mode on [`decide_action`], which enforces
/// `direction_allowed` and contribute's never-delete rule itself.
pub fn decide_action_tri(
    relpath: &str,
    left: &SideState,
    right: &SideState,
    baseline: Option<&FileRecord>,
) -> SyncAction {
    use crate::tri_tree::{NodeState, TriAction, TriConflict, TriState, decide};

    fn node(meta: Option<&crate::types::FileMeta>) -> NodeState {
        match meta {
            Some(m) => NodeState::present(m.blake3, m.size),
            None => NodeState::Absent,
        }
    }

    let state = TriState {
        local: node(left.meta.as_ref()),
        synced: baseline
            .map(|b| NodeState::present(b.blake3, b.size))
            .unwrap_or(NodeState::Absent),
        remote: node(right.meta.as_ref()),
    };

    let relpath = relpath.to_string();
    match decide(&state) {
        // Nothing to transfer. `AdvanceSynced` and `DropFromSynced` are
        // bookkeeping-only outcomes, and the executor already refreshes
        // the baseline for a Noop, so they collapse here.
        TriAction::Noop | TriAction::AdvanceSynced | TriAction::DropFromSynced => {
            SyncAction::Noop { relpath }
        }
        TriAction::PullToLocal => SyncAction::Copy {
            relpath,
            direction: Direction::RightToLeft,
        },
        TriAction::PushToRemote => SyncAction::Copy {
            relpath,
            direction: Direction::LeftToRight,
        },
        TriAction::DeleteLocal => SyncAction::Delete {
            relpath,
            direction: Direction::RightToLeft,
        },
        TriAction::DeleteRemote => SyncAction::Delete {
            relpath,
            direction: Direction::LeftToRight,
        },
        TriAction::Conflict(kind) => {
            // Winner/loser matches Phase 25's convention: the side that
            // still has content wins, and for a two-sided edit the
            // later mtime wins with a tie going to the left.
            let (winner, loser) = match kind {
                TriConflict::DeleteEdit => (Direction::RightToLeft, Direction::LeftToRight),
                TriConflict::EditDelete => (Direction::LeftToRight, Direction::RightToLeft),
                TriConflict::EditEdit | TriConflict::AddAdd => {
                    let lm = left.meta.as_ref().map(|m| m.mtime_ms).unwrap_or(i64::MIN);
                    let rm = right.meta.as_ref().map(|m| m.mtime_ms).unwrap_or(i64::MIN);
                    if rm > lm {
                        (Direction::RightToLeft, Direction::LeftToRight)
                    } else {
                        (Direction::LeftToRight, Direction::RightToLeft)
                    }
                }
            };
            SyncAction::KeepConflict {
                relpath,
                winner,
                loser,
                kind: match kind {
                    TriConflict::EditEdit => ConflictKind::ConcurrentWrite,
                    TriConflict::DeleteEdit | TriConflict::EditDelete => ConflictKind::DeleteEdit,
                    TriConflict::AddAdd => ConflictKind::AddAdd,
                },
            }
        }
    }
}

pub fn decide_action(
    relpath: &str,
    left: &SideState,
    right: &SideState,
    baseline: Option<&FileRecord>,
    mode: SyncMode,
) -> SyncAction {
    let lm = left.meta.as_ref();
    let rm = right.meta.as_ref();

    // Both absent → Noop (+ the caller can prune the baseline).
    if lm.is_none() && rm.is_none() {
        return SyncAction::Noop {
            relpath: relpath.to_string(),
        };
    }

    // Both present with identical content → Noop (advance baseline).
    if let (Some(l), Some(r)) = (lm, rm) {
        if l.blake3 == r.blake3 {
            return SyncAction::Noop {
                relpath: relpath.to_string(),
            };
        }
    }

    let l_matches_baseline = matches_baseline(lm, baseline);
    let r_matches_baseline = matches_baseline(rm, baseline);

    match (lm.is_some(), rm.is_some(), baseline.is_some()) {
        // New file on left only.
        (true, false, false) => direction_allowed(mode, Direction::LeftToRight)
            .map(|_| SyncAction::Copy {
                relpath: relpath.to_string(),
                direction: Direction::LeftToRight,
            })
            .unwrap_or_else(|| SyncAction::Noop {
                relpath: relpath.to_string(),
            }),
        // New file on right only.
        (false, true, false) => direction_allowed(mode, Direction::RightToLeft)
            .map(|_| SyncAction::Copy {
                relpath: relpath.to_string(),
                direction: Direction::RightToLeft,
            })
            .unwrap_or_else(|| SyncAction::Noop {
                relpath: relpath.to_string(),
            }),
        // Both added at the same relpath with different content — add/add conflict.
        (true, true, false) => {
            let (winner, loser) = resolve_conflict_sides(mode, lm.unwrap(), rm.unwrap());
            SyncAction::KeepConflict {
                relpath: relpath.to_string(),
                winner,
                loser,
                kind: ConflictKind::AddAdd,
            }
        }
        // Baseline existed:
        (true, true, true) => {
            let l = lm.unwrap();
            let r = rm.unwrap();
            match (l_matches_baseline, r_matches_baseline) {
                (true, true) => SyncAction::Noop {
                    relpath: relpath.to_string(),
                },
                (true, false) => {
                    // Right changed since baseline.
                    //
                    // Copying R→L is only correct when the mode permits
                    // writing to the left. This arm used to do it for
                    // every mode except Contribute, so a
                    // `MirrorLeftToRight` propagated the *replica's*
                    // edit back over its own authoritative source —
                    // a mirror overwriting the thing it mirrors.
                    match mode {
                        // Contribute donates left→right but never reaches
                        // back to overwrite what the far side did with it.
                        SyncMode::ContributeLeftToRight => SyncAction::Noop {
                            relpath: relpath.to_string(),
                        },
                        // The right is a replica here and it drifted:
                        // restore it from the authoritative left.
                        SyncMode::MirrorLeftToRight => SyncAction::Copy {
                            relpath: relpath.to_string(),
                            direction: Direction::LeftToRight,
                        },
                        // TwoWay, and MirrorRightToLeft where the right
                        // *is* authoritative.
                        _ => SyncAction::Copy {
                            relpath: relpath.to_string(),
                            direction: Direction::RightToLeft,
                        },
                    }
                }
                (false, true) => match mode {
                    // Left is the replica under a right-authoritative
                    // mirror; restore it rather than pushing its edit
                    // onto the source.
                    SyncMode::MirrorRightToLeft => SyncAction::Copy {
                        relpath: relpath.to_string(),
                        direction: Direction::RightToLeft,
                    },
                    _ => SyncAction::Copy {
                        relpath: relpath.to_string(),
                        direction: Direction::LeftToRight,
                    },
                },
                (false, false) => {
                    // Both changed. If the new contents agree, no-op
                    // and advance baseline; if they disagree, conflict.
                    if l.blake3 == r.blake3 {
                        SyncAction::Noop {
                            relpath: relpath.to_string(),
                        }
                    } else {
                        let (winner, loser) = resolve_conflict_sides(mode, l, r);
                        SyncAction::KeepConflict {
                            relpath: relpath.to_string(),
                            winner,
                            loser,
                            kind: ConflictKind::ConcurrentWrite,
                        }
                    }
                }
            }
        }
        // Left deleted (absent, had baseline), right still present.
        (false, true, true) => {
            if r_matches_baseline {
                // Left deleted a file the right still holds unchanged,
                // so the delete applies to the right. Mirror of the
                // `(true, false, true)` arm below.
                if matches!(mode, SyncMode::ContributeLeftToRight) {
                    // Contribute never removes anything on the receiver.
                    SyncAction::Noop {
                        relpath: relpath.to_string(),
                    }
                } else if direction_allowed(mode, Direction::LeftToRight).is_none() {
                    // `MirrorRightToLeft`: the right is authoritative,
                    // so a deletion on the left is not allowed to erase
                    // it — put the file back instead.
                    SyncAction::Copy {
                        relpath: relpath.to_string(),
                        direction: Direction::RightToLeft,
                    }
                } else {
                    SyncAction::Delete {
                        relpath: relpath.to_string(),
                        direction: Direction::LeftToRight,
                    }
                }
            } else {
                // Right edited after last sync, left deleted → delete-edit conflict.
                SyncAction::KeepConflict {
                    relpath: relpath.to_string(),
                    winner: Direction::RightToLeft,
                    loser: Direction::LeftToRight,
                    kind: ConflictKind::DeleteEdit,
                }
            }
        }
        // Left present, right deleted.
        (true, false, true) => {
            if l_matches_baseline {
                // The right dropped a file the left still holds
                // unchanged. Propagating that as a delete writes to the
                // *left*, so it is only legitimate when the mode allows
                // a right-to-left write. Under `ContributeLeftToRight`
                // and `MirrorLeftToRight` the left is authoritative
                // (`SyncMode`'s contract: a left-to-right mirror
                // "deletes extras on right"), so the file is put back
                // instead — otherwise a deletion on the receiver would
                // destroy the sender's only copy.
                if direction_allowed(mode, Direction::RightToLeft).is_none() {
                    SyncAction::Copy {
                        relpath: relpath.to_string(),
                        direction: Direction::LeftToRight,
                    }
                } else {
                    SyncAction::Delete {
                        relpath: relpath.to_string(),
                        direction: Direction::RightToLeft,
                    }
                }
            } else {
                SyncAction::KeepConflict {
                    relpath: relpath.to_string(),
                    winner: Direction::LeftToRight,
                    loser: Direction::RightToLeft,
                    kind: ConflictKind::DeleteEdit,
                }
            }
        }
        // Both absent but baseline had the relpath → prune.
        (false, false, true) => SyncAction::Noop {
            relpath: relpath.to_string(),
        },
        // Unreachable: the both-absent-no-baseline case is handled by
        // the early return at the top of this function. Kept explicit
        // so the match stays total.
        (false, false, false) => SyncAction::Noop {
            relpath: relpath.to_string(),
        },
    }
}

/// Collapse case-variant spellings of the same file onto one relpath.
///
/// On NTFS, exFAT and a default-configured APFS, `Foo.txt` and
/// `foo.txt` are the same file. The two sides are walked
/// independently, so nothing stops them reporting different spellings —
/// a file renamed only in case, or two roots seeded from different
/// sources. With byte-exact keys the engine then saw two files, each
/// absent from the other side, and planned a copy in both directions.
/// Both writes land on the same on-disk file, so the second undoes the
/// first; under a mirror mode the follow-up pass sees the loser
/// missing and deletes it.
///
/// Only the map *key* is folded. `SideState::relpath` keeps a real
/// on-disk spelling, because that is what has to be opened.
///
/// The surviving spelling is the baseline's if it has one, else the
/// left side's, else the right's. Preferring what the database already
/// records keeps the baseline key from churning between runs, which
/// would otherwise re-seed the row every pass.
fn unify_case_variants(
    left: std::collections::BTreeMap<String, SideState>,
    right: std::collections::BTreeMap<String, SideState>,
    db_keys: &[String],
) -> (
    std::collections::BTreeMap<String, SideState>,
    std::collections::BTreeMap<String, SideState>,
) {
    use std::collections::{BTreeMap, HashMap};

    let mut canonical: HashMap<String, String> = HashMap::new();
    // Lowest-priority first, so a later insert overwrites: right, then
    // left, then the baseline.
    for k in right.keys().chain(left.keys()) {
        canonical.insert(fold_relpath(k), k.clone());
    }
    for k in db_keys {
        canonical.insert(fold_relpath(k), k.clone());
    }

    let rekey = |side: BTreeMap<String, SideState>| -> BTreeMap<String, SideState> {
        let mut out: BTreeMap<String, SideState> = BTreeMap::new();
        for (key, mut state) in side {
            let chosen = canonical
                .get(&fold_relpath(&key))
                .cloned()
                .unwrap_or_else(|| key.clone());
            state.relpath = chosen.clone();
            // Two spellings on ONE side can only happen on a volume
            // that is case-sensitive after all — the probe can be
            // wrong about a network mount. Keep the first and leave
            // the other alone rather than silently dropping a file.
            out.entry(chosen).or_insert(state);
        }
        out
    };

    (rekey(left), rekey(right))
}

fn matches_baseline(current: Option<&FileMeta>, baseline: Option<&FileRecord>) -> bool {
    match (current, baseline) {
        (Some(c), Some(b)) => c.blake3 == b.blake3 && c.size == b.size,
        _ => false,
    }
}

fn direction_allowed(mode: SyncMode, direction: Direction) -> Option<()> {
    match (mode, direction) {
        (SyncMode::TwoWay, _) => Some(()),
        (SyncMode::MirrorLeftToRight, Direction::LeftToRight) => Some(()),
        (SyncMode::MirrorRightToLeft, Direction::RightToLeft) => Some(()),
        (SyncMode::ContributeLeftToRight, Direction::LeftToRight) => Some(()),
        _ => None,
    }
}

fn pick_winner_by_mtime(left: &FileMeta, right: &FileMeta) -> (Direction, Direction) {
    // Later mtime wins. Ties → Left for determinism.
    if right.mtime_ms > left.mtime_ms {
        (Direction::RightToLeft, Direction::LeftToRight)
    } else {
        (Direction::LeftToRight, Direction::RightToLeft)
    }
}

/// Resolve a conflict's `(winner, loser)` for `mode`.
///
/// In a one-way mode the authoritative side always wins, regardless of
/// mtime. Deciding by mtime alone was actively destructive: on a first
/// `MirrorLeftToRight` run against a pre-populated destination there is
/// no baseline, so every differing file is an add/add conflict — and
/// copy tools stamp destination mtimes at copy time, so the destination
/// usually looked "newer" and won. `apply_conflict` would then rename
/// the *source* file to `name.sync-conflict-…` and overwrite it from the
/// replica.
///
/// Two-way keeps the mtime heuristic, which is the only signal it has.
fn resolve_conflict_sides(
    mode: SyncMode,
    left: &FileMeta,
    right: &FileMeta,
) -> (Direction, Direction) {
    match mode {
        SyncMode::MirrorLeftToRight | SyncMode::ContributeLeftToRight => {
            (Direction::LeftToRight, Direction::RightToLeft)
        }
        SyncMode::MirrorRightToLeft => (Direction::RightToLeft, Direction::LeftToRight),
        SyncMode::TwoWay => pick_winner_by_mtime(left, right),
    }
}

async fn apply_copy(
    pair: &SyncPair,
    relpath: &str,
    direction: Direction,
    hook: &dyn CopyHookFactory,
) -> Result<()> {
    let (src_root, dst_root) = match direction {
        Direction::LeftToRight => (&pair.left, &pair.right),
        Direction::RightToLeft => (&pair.right, &pair.left),
    };
    let src = join_relpath(src_root, relpath)
        .ok_or_else(|| SyncError::UnsafeRelpath(relpath.to_string()))?;
    let dst = join_relpath(dst_root, relpath)
        .ok_or_else(|| SyncError::UnsafeRelpath(relpath.to_string()))?;

    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| SyncError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }

    run_copy(&src, &dst, hook, relpath).await
}

/// Outcome of [`apply_delete`].
///
/// The engine planned a [`SyncAction::Delete`] based on the walk pass
/// observation that the source side's file was absent. Between scan
/// and apply, the source can be re-created; deleting the destination
/// in that case would silently propagate stale state and lose the
/// re-created content. The plain success path returns `Deleted`; the
/// race path returns `RaceAbortedAsConflict` so the caller can record
/// the relpath in the report's conflict list and emit a
/// [`SyncEvent::Conflict`] event.
enum DeleteOutcome {
    /// Destination no longer present after apply (either we removed
    /// it, or it was already gone).
    Deleted,
    /// The delete was deliberately not performed and nothing changed
    /// on disk — the caller must leave the baseline, the counters and
    /// the history alone.
    ///
    /// Previously this case returned `Deleted`, so a suppressed delete
    /// still pruned the baseline row, incremented `deleted_*`, and
    /// wrote a `HistoryKind::Deleted` audit entry for a file that is
    /// present on both sides. Dropping the baseline row is the
    /// damaging part: the next round sees "no baseline" and re-derives
    /// the pair's state from scratch.
    Skipped,
    /// Source path reappeared with content that differs from the
    /// baseline between scan and apply — the delete was aborted to
    /// preserve the re-created content. Carries the conflict record
    /// the caller surfaces to the UI.
    RaceAbortedAsConflict(Conflict),
}

async fn apply_delete(
    pair: &SyncPair,
    relpath: &str,
    direction: Direction,
    baseline: Option<&FileRecord>,
) -> Result<DeleteOutcome> {
    let (src_root, dst_root) = match direction {
        // The "deleter" side sourced the absence: in a Direction::
        // LeftToRight delete, the LEFT side observed the file gone,
        // so LEFT is the source of the deletion signal and RIGHT is
        // the destination of the propagated delete.
        Direction::LeftToRight => (&pair.left, &pair.right),
        Direction::RightToLeft => (&pair.right, &pair.left),
    };
    let src = join_relpath(src_root, relpath)
        .ok_or_else(|| SyncError::UnsafeRelpath(relpath.to_string()))?;
    let dst = join_relpath(dst_root, relpath)
        .ok_or_else(|| SyncError::UnsafeRelpath(relpath.to_string()))?;

    // Vector-clock re-check at apply time. The decision matrix saw
    // the source absent and the destination unchanged from baseline,
    // and emitted Delete. If the source has since been re-created,
    // the user just rescued / restored / replaced that file, and
    // propagating the delete now would clobber legitimately-newer
    // content. Re-stat the source and, if it now has content that
    // differs from the baseline (i.e., the re-creation produced
    // material the baseline doesn't already cover), abort the delete
    // and surface a delete-edit conflict.
    if src.exists() {
        // Source reappeared. Compute its current content meta so we
        // can compare against the baseline. If the baseline matches
        // the new source state byte-for-byte (rare, but possible if
        // the user rolled the re-creation back to the last-synced
        // bytes), the delete is no longer warranted but no conflict
        // exists either — abort silently as a no-op-style outcome
        // by returning a "corrupt-equal" conflict pinned to the
        // source side so the UI still surfaces the racy edit.
        let new_meta = crate::walker::read_file_meta(&src).ok();
        let differs_from_baseline = match (&new_meta, baseline) {
            (Some(m), Some(b)) => m.blake3 != b.blake3 || m.size != b.size,
            (Some(_), None) => true,
            (None, _) => true,
        };
        if differs_from_baseline {
            // Source was re-created with content that doesn't match
            // the last-synced baseline → treat as delete-edit. The
            // "winner" is the source side (the one with content);
            // the destination's current state is what the original
            // plan would have deleted, so its preservation is moot —
            // but we still emit a conflict record so the user is
            // alerted to the race.
            let loser = direction; // delete propagation target
            let winner = match direction {
                Direction::LeftToRight => Direction::RightToLeft,
                Direction::RightToLeft => Direction::LeftToRight,
            };
            return Ok(DeleteOutcome::RaceAbortedAsConflict(Conflict {
                relpath: relpath.to_string(),
                kind: ConflictKind::DeleteEdit,
                winner,
                loser,
                // The source path is what the user re-created. Use
                // it as the preservation path for UI surfacing —
                // nothing was renamed, but the field has to point
                // at a real file on disk so the UI's "open conflict"
                // affordance has something to open.
                loser_preservation_path: src,
            }));
        }
        // Source's re-created content is byte-identical to the
        // baseline. The user effectively rolled back to last-synced
        // state on the source side; the destination already has
        // that same content, so deleting now would create a
        // baseline desync. Abort silently — the next sync round
        // will see both sides agree and advance the baseline.
        //
        // `Skipped`, not `Deleted`: nothing was removed, so the caller
        // must not prune the baseline row or count a deletion.
        return Ok(DeleteOutcome::Skipped);
    }

    if dst.exists() {
        std::fs::remove_file(&dst).map_err(|e| SyncError::Io {
            path: dst.clone(),
            source: e,
        })?;
    }
    Ok(DeleteOutcome::Deleted)
}

async fn apply_conflict(
    pair: &SyncPair,
    relpath: &str,
    winner: Direction,
    loser: Direction,
    kind: ConflictKind,
    host_label: &str,
    hook: &dyn CopyHookFactory,
) -> Result<Conflict> {
    // Step 1: rename the loser's live file to
    // `name.sync-conflict-YYYYMMDD-HHMMSS-<host>.ext` so the local
    // copy is preserved for the user.
    let loser_root = match loser {
        Direction::LeftToRight => &pair.left,
        Direction::RightToLeft => &pair.right,
    };
    let winner_root = match winner {
        Direction::LeftToRight => &pair.left,
        Direction::RightToLeft => &pair.right,
    };
    let loser_path = join_relpath(loser_root, relpath)
        .ok_or_else(|| SyncError::UnsafeRelpath(relpath.to_string()))?;
    let winner_path = join_relpath(winner_root, relpath)
        .ok_or_else(|| SyncError::UnsafeRelpath(relpath.to_string()))?;

    let preservation_dir = loser_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| loser_root.clone());
    // Pick a preservation filename that doesn't already exist on
    // disk. Two conflicts on the same relpath inside the same second
    // (back-to-back syncs, multi-pair conflicts converging on the
    // same name) would otherwise collide on the timestamp suffix and
    // a rename of the loser would silently overwrite the previous
    // conflict's preservation file. Try the bare timestamp first;
    // if a file already lives at that path, append `-NNNNNN` (six
    // digits, zero-padded) sequence numbers until we find a free
    // slot. Cap at one million attempts so a directory-listing bug
    // can't loop forever.
    let preservation_path = unique_conflict_path(&preservation_dir, relpath, host_label);

    // Delete-edit conflict where loser is the deleter: the loser
    // has nothing to preserve. For the other kinds, rename the
    // loser's file to the preservation path.
    let loser_has_content = matches!(
        kind,
        ConflictKind::ConcurrentWrite | ConflictKind::AddAdd | ConflictKind::CorruptEqual
    ) || (kind == ConflictKind::DeleteEdit && loser_path.exists());

    if loser_has_content && loser_path.exists() {
        // Ensure parent exists (it should — we're renaming in place).
        if let Some(parent) = preservation_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| SyncError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        std::fs::rename(&loser_path, &preservation_path).map_err(|e| SyncError::Io {
            path: loser_path.clone(),
            source: e,
        })?;
    }

    // Step 2: if the winner has content, propagate it to the loser's
    // canonical path. For a delete-edit where the editor won, this
    // copies the winner's edit to the deleter's side. For a delete-edit
    // where the deleter won (rare under mtime wins but possible under
    // ties), the loser's content was already renamed; nothing to copy.
    if winner_path.exists() {
        if let Some(parent) = loser_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| SyncError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        run_copy(&winner_path, &loser_path, hook, relpath).await?;
    }

    Ok(Conflict {
        relpath: relpath.to_string(),
        kind,
        winner,
        loser,
        loser_preservation_path: preservation_path,
    })
}

async fn run_copy(src: &Path, dst: &Path, hook: &dyn CopyHookFactory, relpath: &str) -> Result<()> {
    let opts = hook.build();
    let ctrl = CopyControl::new();
    let (tx, mut rx) = mpsc::channel::<CopyEvent>(64);
    let drainer = tokio::spawn(async move { while rx.recv().await.is_some() {} });

    let src_owned = src.to_path_buf();
    let dst_owned = dst.to_path_buf();
    let res = copy_file(&src_owned, &dst_owned, opts, ctrl, tx).await;
    // Drop the sender side by dropping tx's owner (happens when
    // copy_file returns). The drainer task will exit once rx sees
    // the closed channel.
    let _ = drainer.await;
    match res {
        Ok(_) => Ok(()),
        Err(e) => Err(SyncError::Propagation {
            relpath: relpath.to_string(),
            inner: Box::new(e),
        }),
    }
}

fn conflict_preservation_name(relpath: &str, host_label: &str) -> String {
    conflict_preservation_name_with_seq(relpath, host_label, None)
}

/// Build the conflict preservation filename. When `seq` is supplied,
/// the suffix is `-NNNNNN` (six digits, zero-padded) appended after
/// the host label and before the extension; without `seq`, the bare
/// timestamp+host suffix is produced. The split between the two
/// forms keeps the common case ("nothing else collided this second")
/// looking exactly like the prior format.
fn conflict_preservation_name_with_seq(
    relpath: &str,
    host_label: &str,
    seq: Option<u32>,
) -> String {
    let (stem, ext) = split_last_dot(relpath);
    let ts = Utc
        .timestamp_millis_opt(now_ms() as i64)
        .single()
        .unwrap_or_else(Utc::now)
        .format("%Y%m%d-%H%M%S")
        .to_string();
    // The relpath may be `sub/dir/file.ext` — we only want the final
    // component's stem/ext, since we join against the loser's parent
    // dir. `split_last_dot` already operates on the whole relpath;
    // strip the leading directory segments here.
    let last_slash = stem.rfind('/').map(|i| i + 1).unwrap_or(0);
    let file_stem = &stem[last_slash..];
    let seq_suffix = match seq {
        Some(n) => format!("-{n:06}"),
        None => String::new(),
    };
    if let Some(ext) = ext {
        format!("{file_stem}.sync-conflict-{ts}-{host_label}{seq_suffix}.{ext}")
    } else {
        format!("{file_stem}.sync-conflict-{ts}-{host_label}{seq_suffix}")
    }
}

/// Resolve a preservation filename inside `dir` that doesn't collide
/// with an existing entry. Tries the unsuffixed timestamp form first
/// (matches pre-fix behaviour for the overwhelming majority of runs);
/// if that path already exists, retries with `-000001`, `-000002`,
/// ... up to a hard cap that keeps a hostile / corrupt directory from
/// looping the sync run.
fn unique_conflict_path(dir: &Path, relpath: &str, host_label: &str) -> PathBuf {
    const MAX_SEQ: u32 = 1_000_000;
    let bare = dir.join(conflict_preservation_name(relpath, host_label));
    if !bare.exists() {
        return bare;
    }
    for seq in 1..=MAX_SEQ {
        let candidate = dir.join(conflict_preservation_name_with_seq(
            relpath,
            host_label,
            Some(seq),
        ));
        if !candidate.exists() {
            return candidate;
        }
    }
    // Astronomically unlikely fall-through. Returning the last-tried
    // candidate is safer than panicking — the rename below will fail
    // with a typed I/O error and the engine surfaces it cleanly. We
    // pick a sentinel that includes the cap so a forensic investigator
    // can spot the saturation in a log.
    dir.join(conflict_preservation_name_with_seq(
        relpath,
        host_label,
        Some(MAX_SEQ),
    ))
}

fn split_last_dot(s: &str) -> (&str, Option<&str>) {
    // Split at the LAST dot that is inside the final path segment.
    // `foo.bar/baz` → ("foo.bar/baz", None) because the only dot is
    // before a separator. `foo.tar.gz` → ("foo.tar", Some("gz")).
    let last_slash = s.rfind('/').map(|i| i + 1).unwrap_or(0);
    let last_segment = &s[last_slash..];
    match last_segment.rfind('.') {
        Some(i) if i > 0 && i < last_segment.len() - 1 => {
            let abs_i = last_slash + i;
            (&s[..abs_i], Some(&s[abs_i + 1..]))
        }
        _ => (s, None),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn elapsed_ms(start: SystemTime) -> u64 {
    SystemTime::now()
        .duration_since(start)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn short_device(id: uuid::Uuid) -> String {
    id.to_string().chars().take(8).collect()
}

/// Default copy-hook factory that builds a minimal [`CopyOptions`]
/// honouring the sync-level [`SyncOptions`]. The Tauri runner
/// overrides this with a factory that attaches platform hooks.
#[derive(Debug)]
struct DefaultCopyHookFactory {
    buffer_size: usize,
    fsync_on_close: bool,
}

impl DefaultCopyHookFactory {
    fn new(opts: &SyncOptions) -> Self {
        Self {
            buffer_size: opts.copy_buffer_size,
            fsync_on_close: opts.fsync_on_close,
        }
    }
}

impl CopyHookFactory for DefaultCopyHookFactory {
    fn build(&self) -> CopyOptions {
        CopyOptions {
            buffer_size: self.buffer_size,
            fsync_on_close: self.fsync_on_close,
            ..CopyOptions::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::VersionVector;

    fn meta(blake3_byte: u8, mtime_ms: i64) -> FileMeta {
        FileMeta {
            mtime_ms,
            size: 4,
            blake3: [blake3_byte; 32],
        }
    }

    fn side_state(relpath: &str, meta: Option<FileMeta>) -> SideState {
        SideState {
            relpath: relpath.to_string(),
            meta,
        }
    }

    fn baseline(blake3_byte: u8) -> FileRecord {
        FileRecord {
            vv: VersionVector::new(),
            mtime_ms: 0,
            size: 4,
            blake3: [blake3_byte; 32],
        }
    }

    fn side_map(
        entries: &[(&str, Option<FileMeta>)],
    ) -> std::collections::BTreeMap<String, SideState> {
        entries
            .iter()
            .map(|(p, m)| ((*p).to_string(), side_state(p, m.clone())))
            .collect()
    }

    /// `Foo.txt` on one side and `foo.txt` on the other are one file on
    /// a case-insensitive volume. Keyed byte-exactly they looked like
    /// two files, each absent from the other side, so the engine
    /// planned a copy in both directions onto the same on-disk file —
    /// and a mirror pass then deleted the loser.
    #[test]
    fn case_variants_collapse_to_one_relpath() {
        let left = side_map(&[("Photos/Foo.txt", Some(meta(1, 100)))]);
        let right = side_map(&[("photos/foo.txt", Some(meta(1, 100)))]);

        let (l, r) = unify_case_variants(left, right, &[]);

        assert_eq!(l.len(), 1);
        assert_eq!(r.len(), 1);
        let lk = l.keys().next().unwrap();
        let rk = r.keys().next().unwrap();
        assert_eq!(lk, rk, "both sides must land on one key");
        assert_eq!(
            lk, "Photos/Foo.txt",
            "with no baseline, the left spelling wins"
        );
        // The stored relpath is a real on-disk spelling, not the fold.
        assert_eq!(r.values().next().unwrap().relpath, "Photos/Foo.txt");
    }

    /// The baseline spelling wins over both sides, so the database key
    /// does not churn from run to run and re-seed the row every pass.
    #[test]
    fn the_baseline_spelling_is_preferred() {
        let left = side_map(&[("Report.PDF", Some(meta(1, 100)))]);
        let right = side_map(&[("report.pdf", Some(meta(1, 100)))]);
        let db_keys = vec!["RePoRt.pdf".to_string()];

        let (l, r) = unify_case_variants(left, right, &db_keys);

        assert_eq!(l.keys().next().unwrap(), "RePoRt.pdf");
        assert_eq!(r.keys().next().unwrap(), "RePoRt.pdf");
    }

    /// Distinct names must not be merged just because they share a
    /// prefix or differ by more than case.
    #[test]
    fn genuinely_different_names_stay_separate() {
        let left = side_map(&[
            ("a.txt", Some(meta(1, 100))),
            ("a2.txt", Some(meta(2, 100))),
            ("dir/a.txt", Some(meta(3, 100))),
        ]);
        let (l, _r) = unify_case_variants(left, side_map(&[]), &[]);
        assert_eq!(
            l.len(),
            3,
            "no over-merging: {:?}",
            l.keys().collect::<Vec<_>>()
        );
    }

    /// If the probe is wrong about a volume — a case-sensitive network
    /// mount, say — one side can legitimately hold both spellings.
    /// Dropping one would lose a file, so the first is kept and the
    /// second left where it is rather than silently discarded.
    #[test]
    fn two_spellings_on_one_side_do_not_lose_a_file() {
        let left = side_map(&[
            ("Foo.txt", Some(meta(1, 100))),
            ("foo.txt", Some(meta(2, 200))),
        ]);
        let (l, _r) = unify_case_variants(left, side_map(&[]), &[]);
        assert_eq!(l.len(), 1, "they fold onto one key by construction");
        // Whichever survives must carry a real spelling.
        let kept = l.values().next().unwrap();
        assert!(kept.relpath == "Foo.txt" || kept.relpath == "foo.txt");
    }

    #[test]
    fn both_absent_is_noop() {
        let a = side_state("a", None);
        let b = side_state("a", None);
        let action = decide_action("a", &a, &b, None, SyncMode::TwoWay);
        assert!(matches!(action, SyncAction::Noop { .. }));
    }

    #[test]
    fn both_identical_is_noop() {
        let a = side_state("a", Some(meta(1, 10)));
        let b = side_state("a", Some(meta(1, 20)));
        let action = decide_action("a", &a, &b, None, SyncMode::TwoWay);
        assert!(matches!(action, SyncAction::Noop { .. }));
    }

    #[test]
    fn new_file_on_left_copies_right() {
        let a = side_state("a", Some(meta(1, 10)));
        let b = side_state("a", None);
        let action = decide_action("a", &a, &b, None, SyncMode::TwoWay);
        assert_eq!(
            action,
            SyncAction::Copy {
                relpath: "a".into(),
                direction: Direction::LeftToRight,
            }
        );
    }

    #[test]
    fn new_file_on_right_copies_left() {
        let a = side_state("a", None);
        let b = side_state("a", Some(meta(1, 10)));
        let action = decide_action("a", &a, &b, None, SyncMode::TwoWay);
        assert_eq!(
            action,
            SyncAction::Copy {
                relpath: "a".into(),
                direction: Direction::RightToLeft,
            }
        );
    }

    #[test]
    fn left_unchanged_right_edited_propagates_right_to_left() {
        let a = side_state("a", Some(meta(1, 10)));
        let b = side_state("a", Some(meta(2, 20)));
        let action = decide_action("a", &a, &b, Some(&baseline(1)), SyncMode::TwoWay);
        assert_eq!(
            action,
            SyncAction::Copy {
                relpath: "a".into(),
                direction: Direction::RightToLeft,
            }
        );
    }

    #[test]
    fn left_edited_right_unchanged_propagates_left_to_right() {
        let a = side_state("a", Some(meta(3, 20)));
        let b = side_state("a", Some(meta(1, 10)));
        let action = decide_action("a", &a, &b, Some(&baseline(1)), SyncMode::TwoWay);
        assert_eq!(
            action,
            SyncAction::Copy {
                relpath: "a".into(),
                direction: Direction::LeftToRight,
            }
        );
    }

    #[test]
    fn both_edited_differently_is_conflict() {
        let a = side_state("a", Some(meta(5, 10)));
        let b = side_state("a", Some(meta(7, 20)));
        let action = decide_action("a", &a, &b, Some(&baseline(1)), SyncMode::TwoWay);
        match action {
            SyncAction::KeepConflict { kind, winner, .. } => {
                assert_eq!(kind, ConflictKind::ConcurrentWrite);
                // Right has later mtime, so right wins.
                assert_eq!(winner, Direction::RightToLeft);
            }
            other => panic!("expected KeepConflict, got {other:?}"),
        }
    }

    #[test]
    fn left_deleted_right_unchanged_propagates_delete() {
        let a = side_state("a", None);
        let b = side_state("a", Some(meta(1, 10)));
        let action = decide_action("a", &a, &b, Some(&baseline(1)), SyncMode::TwoWay);
        assert_eq!(
            action,
            SyncAction::Delete {
                relpath: "a".into(),
                direction: Direction::LeftToRight,
            }
        );
    }

    #[test]
    fn delete_edit_is_conflict() {
        let a = side_state("a", None);
        let b = side_state("a", Some(meta(9, 99)));
        let action = decide_action("a", &a, &b, Some(&baseline(1)), SyncMode::TwoWay);
        match action {
            SyncAction::KeepConflict { kind, winner, .. } => {
                assert_eq!(kind, ConflictKind::DeleteEdit);
                assert_eq!(winner, Direction::RightToLeft);
            }
            other => panic!("expected KeepConflict, got {other:?}"),
        }
    }

    #[test]
    fn add_add_different_content_is_conflict() {
        let a = side_state("a", Some(meta(1, 10)));
        let b = side_state("a", Some(meta(2, 20)));
        let action = decide_action("a", &a, &b, None, SyncMode::TwoWay);
        match action {
            SyncAction::KeepConflict { kind, .. } => {
                assert_eq!(kind, ConflictKind::AddAdd);
            }
            other => panic!("expected KeepConflict, got {other:?}"),
        }
    }

    #[test]
    fn contribute_mode_never_deletes_left() {
        let a = side_state("a", Some(meta(1, 10)));
        let b = side_state("a", None);
        // Baseline matches left → left unchanged, right deleted.
        let action = decide_action(
            "a",
            &a,
            &b,
            Some(&baseline(1)),
            SyncMode::ContributeLeftToRight,
        );
        // Contribute reinterprets as "re-copy left to right".
        assert_eq!(
            action,
            SyncAction::Copy {
                relpath: "a".into(),
                direction: Direction::LeftToRight,
            }
        );
    }

    #[test]
    fn contribute_mode_ignores_right_edits() {
        let a = side_state("a", Some(meta(1, 10)));
        let b = side_state("a", Some(meta(5, 50)));
        let action = decide_action(
            "a",
            &a,
            &b,
            Some(&baseline(1)),
            SyncMode::ContributeLeftToRight,
        );
        assert!(matches!(action, SyncAction::Noop { .. }));
    }

    /// Regression — a one-way mode must never delete on its own source.
    ///
    /// [`SyncMode`]'s contract is that `MirrorLeftToRight` "deletes
    /// extras on right". The delete arms were ungated, so a file removed
    /// on the *receiver* came back as a delete aimed at the *sender*:
    /// tidying up the destination silently destroyed the source's only
    /// copy on the next run. Neither mirror mode had any coverage.
    #[test]
    fn a_one_way_mode_never_deletes_on_its_own_source() {
        // Receiver dropped a file the sender still holds, unchanged.
        let sender = side_state("a", Some(meta(1, 10)));
        let receiver = side_state("a", None);
        for mode in [SyncMode::MirrorLeftToRight, SyncMode::ContributeLeftToRight] {
            let action = decide_action("a", &sender, &receiver, Some(&baseline(1)), mode);
            assert_eq!(
                action,
                SyncAction::Copy {
                    relpath: "a".into(),
                    direction: Direction::LeftToRight,
                },
                "{mode:?} must restore the receiver's copy, not delete the sender's"
            );
        }

        // The same case with the sides swapped.
        let action = decide_action(
            "a",
            &side_state("a", None),
            &side_state("a", Some(meta(1, 10))),
            Some(&baseline(1)),
            SyncMode::MirrorRightToLeft,
        );
        assert_eq!(
            action,
            SyncAction::Copy {
                relpath: "a".into(),
                direction: Direction::RightToLeft,
            },
            "a right-to-left mirror must restore, not delete its source"
        );
    }

    /// The counterpart: deletes a mode *is* allowed to make still happen,
    /// so the gate above did not simply disable deletion.
    #[test]
    fn deletes_the_mode_allows_still_propagate() {
        let present = side_state("a", Some(meta(1, 10)));
        let absent = side_state("a", None);

        // Two-way propagates a deletion in either direction.
        assert_eq!(
            decide_action("a", &present, &absent, Some(&baseline(1)), SyncMode::TwoWay),
            SyncAction::Delete {
                relpath: "a".into(),
                direction: Direction::RightToLeft,
            }
        );

        // A left-to-right mirror deletes extras on the right — the
        // behaviour `SyncMode` documents.
        assert_eq!(
            decide_action(
                "a",
                &absent,
                &present,
                Some(&baseline(1)),
                SyncMode::MirrorLeftToRight
            ),
            SyncAction::Delete {
                relpath: "a".into(),
                direction: Direction::LeftToRight,
            }
        );

        // And a right-to-left mirror deletes extras on the left.
        assert_eq!(
            decide_action(
                "a",
                &present,
                &absent,
                Some(&baseline(1)),
                SyncMode::MirrorRightToLeft
            ),
            SyncAction::Delete {
                relpath: "a".into(),
                direction: Direction::RightToLeft,
            }
        );
    }

    #[test]
    fn split_last_dot_keeps_multi_dot_stem_intact() {
        let (stem, ext) = split_last_dot("foo.tar.gz");
        assert_eq!(stem, "foo.tar");
        assert_eq!(ext, Some("gz"));
    }

    #[test]
    fn split_last_dot_returns_none_for_no_ext() {
        let (stem, ext) = split_last_dot("README");
        assert_eq!(stem, "README");
        assert_eq!(ext, None);
    }

    #[test]
    fn split_last_dot_ignores_dot_before_slash() {
        let (stem, ext) = split_last_dot("foo.bar/baz");
        assert_eq!(stem, "foo.bar/baz");
        assert_eq!(ext, None);
    }

    #[test]
    fn split_last_dot_handles_subdirs() {
        let (stem, ext) = split_last_dot("sub/dir/file.ext");
        assert_eq!(stem, "sub/dir/file");
        assert_eq!(ext, Some("ext"));
    }

    #[test]
    fn conflict_name_has_timestamp_host_and_ext() {
        let name = conflict_preservation_name("sub/file.txt", "A");
        assert!(name.starts_with("file.sync-conflict-"));
        assert!(name.ends_with("-A.txt"));
    }

    #[test]
    fn conflict_name_without_ext() {
        let name = conflict_preservation_name("README", "A");
        assert!(name.starts_with("README.sync-conflict-"));
        assert!(name.ends_with("-A"));
        // No trailing `.<ext>` — the loser's final segment had no
        // `.ext` portion to preserve.
        assert!(!name.rsplit_once('-').unwrap().1.contains('.'));
    }

    #[test]
    fn pick_winner_later_mtime() {
        let a = meta(1, 100);
        let b = meta(2, 200);
        let (w, l) = pick_winner_by_mtime(&a, &b);
        assert_eq!(w, Direction::RightToLeft);
        assert_eq!(l, Direction::LeftToRight);
    }

    #[test]
    fn pick_winner_ties_go_to_left() {
        let a = meta(1, 100);
        let b = meta(2, 100);
        let (w, _) = pick_winner_by_mtime(&a, &b);
        assert_eq!(w, Direction::LeftToRight);
    }

    #[test]
    fn unique_conflict_path_returns_bare_when_dir_empty() {
        let d = tempfile::tempdir().unwrap();
        let path = unique_conflict_path(d.path(), "file.txt", "A");
        // Bare suffix has no `-NNNNNN` sequence number — the file
        // name should match the unsuffixed pattern exactly. The
        // bare form ends with `-<host>.<ext>`, so simply checking
        // the bare-name equality is the cleanest assertion.
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let expected = conflict_preservation_name("file.txt", "A");
        assert_eq!(name, expected);
    }

    #[test]
    fn unique_conflict_path_bumps_when_bare_exists() {
        // Round-trip through the resolver itself to dodge wall-clock
        // races: take the path the resolver picks first, write it,
        // then ask again and assert the second path bumped (carries
        // a six-digit seq segment between host and ext). This holds
        // whether or not the second resolver call landed on the
        // same wall-clock second — because the previous file's name
        // already covers the bare slot for that second.
        let d = tempfile::tempdir().unwrap();
        let p0 = unique_conflict_path(d.path(), "file.txt", "A");
        std::fs::write(&p0, b"existing").unwrap();
        let p1 = unique_conflict_path(d.path(), "file.txt", "A");
        assert_ne!(p1, p0);
        assert!(!p1.exists());
        let p1_name = p1.file_name().unwrap().to_string_lossy().to_string();
        // If the second call landed on the same second as p0, the
        // resolver had to bump and the name must carry -A-NNNNNN.
        // If the wall clock ticked over to a new second, the new
        // bare name is enough — but the test wants to exercise the
        // bump path, so set up a consistent fixture: write p1's
        // name, ask a third time, and at THAT round (the dir now
        // has p0+p1), the resolver MUST produce a bumped name no
        // matter which second it lands on.
        std::fs::write(&p1, b"existing-1").unwrap();
        let p2 = unique_conflict_path(d.path(), "file.txt", "A");
        assert_ne!(p2, p0);
        assert_ne!(p2, p1);
        assert!(!p2.exists());
        // Sanity-check the format: at least one of p1 / p2 carries
        // the bump suffix `-A-NNNNNN.<ext>` (six-digit zero-padded
        // sequence between the host label and the extension).
        let p2_name = p2.file_name().unwrap().to_string_lossy().to_string();
        let any_bumped = [&p1_name, &p2_name].iter().any(|n: &&String| {
            // Look for `-A-` followed by exactly six digits then
            // `.txt`.
            if let Some(idx) = n.find("-A-") {
                let tail = &n[idx + 3..];
                tail.len() >= 10
                    && tail.as_bytes()[..6].iter().all(|b: &u8| b.is_ascii_digit())
                    && tail[6..].starts_with(".txt")
            } else {
                false
            }
        });
        assert!(
            any_bumped,
            "expected at least one bumped name with -A-NNNNNN.txt suffix; got p1={p1_name}, p2={p2_name}",
        );
    }

    #[tokio::test]
    async fn apply_delete_aborts_when_source_recreated_with_new_content() {
        // Set up a synthetic pair: source side LEFT was observed
        // absent during the walk, destination side RIGHT carries the
        // last-synced bytes. Between scan and apply, LEFT is
        // re-created with content that doesn't match the baseline
        // — apply_delete must surface a delete-edit conflict and
        // refuse to remove the destination.
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        // Write the destination's "last-synced" content + a baseline
        // hash that matches it.
        std::fs::write(right.path().join("doc.txt"), b"baseline-bytes").unwrap();
        let baseline_blake3 = *blake3::hash(b"baseline-bytes").as_bytes();
        let baseline_record = FileRecord {
            vv: VersionVector::new(),
            mtime_ms: 0,
            size: b"baseline-bytes".len() as u64,
            blake3: baseline_blake3,
        };
        // Race: LEFT gets re-created with newer + different bytes
        // before apply runs.
        std::fs::write(left.path().join("doc.txt"), b"newer-rescued-bytes").unwrap();
        let pair = SyncPair::new("p", left.path(), right.path());
        let outcome = apply_delete(
            &pair,
            "doc.txt",
            Direction::LeftToRight,
            Some(&baseline_record),
        )
        .await
        .expect("apply_delete returns Ok with race outcome");
        match outcome {
            DeleteOutcome::RaceAbortedAsConflict(c) => {
                assert_eq!(c.kind, ConflictKind::DeleteEdit);
                assert_eq!(c.relpath, "doc.txt");
                assert_eq!(c.winner, Direction::RightToLeft);
                assert_eq!(c.loser, Direction::LeftToRight);
            }
            DeleteOutcome::Deleted | DeleteOutcome::Skipped => {
                panic!("expected RaceAbortedAsConflict; the dest should NOT have been removed")
            }
        }
        // Destination must still be intact.
        assert!(right.path().join("doc.txt").exists());
    }

    #[tokio::test]
    async fn apply_delete_proceeds_when_source_truly_absent() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        std::fs::write(right.path().join("doc.txt"), b"baseline-bytes").unwrap();
        let pair = SyncPair::new("p", left.path(), right.path());
        let outcome = apply_delete(&pair, "doc.txt", Direction::LeftToRight, None)
            .await
            .expect("apply_delete returns Ok");
        assert!(matches!(outcome, DeleteOutcome::Deleted));
        // Destination is gone.
        assert!(!right.path().join("doc.txt").exists());
    }

    #[tokio::test]
    async fn apply_delete_skips_when_source_recreated_with_baseline_bytes() {
        // Edge case: source was re-created but with byte-identical
        // content to the last-synced baseline. There's no real
        // conflict (nothing was lost), but the delete should still
        // be skipped — both sides now have identical content, and
        // the next sync round will collapse them via the normal
        // matrix.
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        std::fs::write(right.path().join("doc.txt"), b"same-bytes").unwrap();
        std::fs::write(left.path().join("doc.txt"), b"same-bytes").unwrap();
        let baseline_record = FileRecord {
            vv: VersionVector::new(),
            mtime_ms: 0,
            size: b"same-bytes".len() as u64,
            blake3: *blake3::hash(b"same-bytes").as_bytes(),
        };
        let pair = SyncPair::new("p", left.path(), right.path());
        let outcome = apply_delete(
            &pair,
            "doc.txt",
            Direction::LeftToRight,
            Some(&baseline_record),
        )
        .await
        .expect("apply_delete returns Ok");
        // `Skipped`, not `Deleted`. This branch never removes the
        // destination — the source reappearing with baseline bytes
        // means we back off and let the next round reconcile — and it
        // used to say `Deleted` anyway, which made the caller prune the
        // baseline row, count a deletion that did not happen, and write
        // a `HistoryKind::Deleted` audit entry for a file present on
        // both sides. Losing the baseline row is the damaging part: the
        // next round then re-derives the pair from scratch.
        assert!(matches!(outcome, DeleteOutcome::Skipped));
        assert!(right.path().join("doc.txt").exists());
        assert!(left.path().join("doc.txt").exists());
    }

    #[test]
    fn unique_conflict_path_walks_through_existing_seqs() {
        // The resolver depends on the current wall clock for the
        // timestamp portion of the filename, so pre-creating by
        // name (without a synchronisation primitive) is racy if the
        // wall clock advances between the setup writes and the
        // resolver call. Instead, let the resolver pick the bare
        // name, then loop: pre-create the resolver's last guess
        // and ask again, asserting the suffix advances each round.
        let d = tempfile::tempdir().unwrap();
        // Round 1: empty dir → bare form.
        let p0 = unique_conflict_path(d.path(), "file.txt", "A");
        std::fs::write(&p0, b"existing-0").unwrap();
        // Round 2: bare exists → resolver must produce a new name
        // that doesn't collide. We don't pin the exact timestamp
        // because the wall clock may advance between rounds; we DO
        // pin the property "the new path is different and free".
        let p1 = unique_conflict_path(d.path(), "file.txt", "A");
        assert_ne!(p1, p0);
        assert!(!p1.exists());
        std::fs::write(&p1, b"existing-1").unwrap();
        // Round 3: same property — third call advances again.
        let p2 = unique_conflict_path(d.path(), "file.txt", "A");
        assert_ne!(p2, p0);
        assert_ne!(p2, p1);
        assert!(!p2.exists());
    }
}

#[cfg(test)]
mod tri_parity_tests {
    //! Phase 52 — the three-tree machine must agree with the shipped
    //! Phase 25 matrix on every unambiguous case.
    //!
    //! This is the evidence for flipping `SyncOptions::tri_tree` on real
    //! data: if the two engines disagreed on a plain one-sided edit, the
    //! new one would be a regression rather than an improvement.

    use super::*;
    use crate::types::FileMeta;

    /// (name, left, right, baseline) — named so the case table
    /// stays readable.
    type Case = (
        &'static str,
        Option<FileMeta>,
        Option<FileMeta>,
        Option<FileRecord>,
    );

    fn meta(hash: u8, mtime: i64) -> FileMeta {
        FileMeta {
            mtime_ms: mtime,
            size: 1,
            blake3: [hash; 32],
        }
    }

    fn side(relpath: &str, m: Option<FileMeta>) -> SideState {
        SideState {
            relpath: relpath.to_string(),
            meta: m,
        }
    }

    fn baseline(hash: u8) -> FileRecord {
        FileRecord {
            vv: Default::default(),
            mtime_ms: 0,
            size: 1,
            blake3: [hash; 32],
        }
    }

    #[test]
    fn agrees_on_one_sided_edits_and_deletes() {
        let cases: Vec<Case> = vec![
            // Left edited, right untouched -> push left to right.
            (
                "edit-left",
                Some(meta(2, 20)),
                Some(meta(1, 10)),
                Some(baseline(1)),
            ),
            // Right edited, left untouched -> pull right to left.
            (
                "edit-right",
                Some(meta(1, 10)),
                Some(meta(2, 20)),
                Some(baseline(1)),
            ),
            // Left deleted, right untouched -> delete right.
            ("del-left", None, Some(meta(1, 10)), Some(baseline(1))),
            // Right deleted, left untouched -> delete left.
            ("del-right", Some(meta(1, 10)), None, Some(baseline(1))),
            // New file on the left only -> push.
            ("new-left", Some(meta(3, 30)), None, None),
            // New file on the right only -> pull.
            ("new-right", None, Some(meta(3, 30)), None),
            // Identical on both sides -> nothing.
            (
                "same",
                Some(meta(1, 10)),
                Some(meta(1, 99)),
                Some(baseline(1)),
            ),
        ];

        for (name, l, r, base) in cases {
            let left = side(name, l);
            let right = side(name, r);
            let old = decide_action(name, &left, &right, base.as_ref(), SyncMode::TwoWay);
            let new = decide_action_tri(name, &left, &right, base.as_ref());
            assert_eq!(
                old, new,
                "engines disagreed on the unambiguous case `{name}`"
            );
        }
    }

    #[test]
    fn both_engines_flag_a_two_sided_edit_as_a_conflict() {
        let left = side("c", Some(meta(2, 20)));
        let right = side("c", Some(meta(3, 30)));
        let base = baseline(1);

        let old = decide_action("c", &left, &right, Some(&base), SyncMode::TwoWay);
        let new = decide_action_tri("c", &left, &right, Some(&base));
        assert!(matches!(old, SyncAction::KeepConflict { .. }));
        assert!(matches!(new, SyncAction::KeepConflict { .. }));
        // And the later mtime wins in both, so the surviving content
        // does not depend on which engine ran.
        if let (
            SyncAction::KeepConflict { winner: w_old, .. },
            SyncAction::KeepConflict { winner: w_new, .. },
        ) = (old, new)
        {
            assert_eq!(w_old, w_new);
        }
    }

    #[test]
    fn converged_content_costs_no_transfer() {
        // Both sides independently reached identical bytes. The old
        // matrix already treats this as a no-op; the three-tree machine
        // must not regress it into a needless copy.
        let left = side("x", Some(meta(9, 20)));
        let right = side("x", Some(meta(9, 30)));
        let base = baseline(1);
        assert_eq!(
            decide_action_tri("x", &left, &right, Some(&base)),
            SyncAction::Noop {
                relpath: "x".to_string()
            }
        );
    }

    #[test]
    fn delete_edit_is_a_conflict_not_a_silent_delete() {
        // The case that most needs to be right: one side deleted while
        // the other edited. Losing the edit here is unrecoverable data
        // loss, so it must surface rather than propagate the delete.
        let left = side("d", None);
        let right = side("d", Some(meta(5, 50)));
        let base = baseline(1);
        let action = decide_action_tri("d", &left, &right, Some(&base));
        match action {
            SyncAction::KeepConflict { kind, winner, .. } => {
                assert_eq!(kind, ConflictKind::DeleteEdit);
                // The side that still has content wins.
                assert_eq!(winner, Direction::RightToLeft);
            }
            other => panic!("expected a conflict, got {other:?}"),
        }
    }
}
