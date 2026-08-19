//! `freally repo verify|repair` — Phase 49n's CLI half.
//!
//! The GUI has had Verify / Verify-deep / Repair buttons since 49n
//! shipped; this exposes the same two `Repository` primitives to
//! scripts and cron so an unattended host can check its own repository
//! without a desktop session.
//!
//! Exit codes follow the crate matrix: a clean verify is `Success`,
//! damage found is `VerifyFailed` (4) so a caller can branch on
//! "repository is damaged" distinctly from "the command itself broke"
//! (`GenericError`).

use std::sync::Arc;

use freally_chunk::{SnapshotId, VerifyLevel, VerifyReport};

use crate::ExitCode;
use crate::cli::{GlobalArgs, RepoArgs, RepoOp};
use crate::output::{JsonEventKind, OutputWriter};

use super::open_repo;

/// One human-readable line per damage entry, capped so a wholly
/// corrupt repository does not print a million lines.
const MAX_DAMAGE_LINES: usize = 50;

fn report_damage(writer: &Arc<OutputWriter>, report: &VerifyReport) {
    for d in report.damage.iter().take(MAX_DAMAGE_LINES) {
        let _ = writer.human(&format!(
            "  damaged: snapshot {} · {} · {:?}",
            d.snapshot_id, d.path, d.kind
        ));
    }
    if report.damage.len() > MAX_DAMAGE_LINES {
        let _ = writer.human(&format!(
            "  … and {} more",
            report.damage.len() - MAX_DAMAGE_LINES
        ));
    }
}

pub(crate) async fn run(
    _global: &GlobalArgs,
    args: RepoArgs,
    writer: Arc<OutputWriter>,
) -> ExitCode {
    match args.op {
        RepoOp::Verify { deep, snapshot } => {
            let repo = match open_repo() {
                Ok(r) => r,
                Err(message) => {
                    writer.error(&message, ExitCode::GenericError.as_u8());
                    return ExitCode::GenericError;
                }
            };
            let level = if deep {
                VerifyLevel::ReadData
            } else {
                VerifyLevel::Metadata
            };
            let only = snapshot.map(SnapshotId);
            let w = writer.clone();
            let report = repo.verify_with_progress(only, level, &mut |p| {
                // A deep pass re-hashes every chunk; without this the
                // command looks hung on a large repository.
                let _ = w.human(&format!("{}: {}/{}", p.phase, p.done, p.total));
            });
            let report = match report {
                Ok(r) => r,
                Err(e) => {
                    writer.error(&format!("verify: {e}"), ExitCode::GenericError.as_u8());
                    return ExitCode::GenericError;
                }
            };
            // `verify` filters by id, so an id that does not exist
            // inspects nothing and comes back with empty damage — i.e.
            // "clean". A cron job checking one snapshot after a typo,
            // or after `repo repair --apply` quarantined it, would
            // record a pass for something that is not there.
            if let Some(id) = snapshot
                && report.snapshots_checked == 0
            {
                writer.error(
                    &format!("snapshot {id} not found"),
                    ExitCode::GenericError.as_u8(),
                );
                return ExitCode::GenericError;
            }
            let _ = writer.human(&format!(
                "verified {} snapshot(s), {} file(s), {} chunk(s)",
                report.snapshots_checked, report.files_checked, report.chunks_checked
            ));
            if report.is_clean() {
                let _ = writer.emit(JsonEventKind::Info {
                    message: "repository verify: clean".to_string(),
                });
                ExitCode::Success
            } else {
                report_damage(&writer, &report);
                writer.error(
                    &format!("repository verify: {} damaged item(s)", report.damage.len()),
                    ExitCode::VerifyFailed.as_u8(),
                );
                ExitCode::VerifyFailed
            }
        }
        RepoOp::Repair { apply, deep } => {
            let repo = match open_repo() {
                Ok(r) => r,
                Err(message) => {
                    writer.error(&message, ExitCode::GenericError.as_u8());
                    return ExitCode::GenericError;
                }
            };
            let level = if deep {
                VerifyLevel::ReadData
            } else {
                VerifyLevel::Metadata
            };
            let report = match repo.verify(None, level) {
                Ok(r) => r,
                Err(e) => {
                    writer.error(
                        &format!("verify before repair: {e}"),
                        ExitCode::GenericError.as_u8(),
                    );
                    return ExitCode::GenericError;
                }
            };
            if report.is_clean() {
                let _ = writer.human("nothing to repair — repository is clean");
                return ExitCode::Success;
            }
            match repo.repair_remove_damaged(&report, apply) {
                Ok((ids, gc)) => {
                    if apply {
                        let _ = writer.human(&format!(
                            "quarantined {} snapshot(s); reclaimed {} byte(s)",
                            ids.len(),
                            gc.bytes_reclaimed
                        ));
                        ExitCode::Success
                    } else {
                        // Dry run is the default so an unattended
                        // caller cannot delete snapshots by typo.
                        let _ = writer.human(&format!(
                            "dry run — {} snapshot(s) would be quarantined; re-run with --apply",
                            ids.len()
                        ));
                        ExitCode::VerifyFailed
                    }
                }
                Err(e) => {
                    writer.error(&format!("repair: {e}"), ExitCode::GenericError.as_u8());
                    ExitCode::GenericError
                }
            }
        }
    }
}
