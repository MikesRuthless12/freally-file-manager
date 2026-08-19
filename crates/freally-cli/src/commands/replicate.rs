//! `freally replicate <src> <dst>` — Phase 50h's CLI half.
//!
//! Push every snapshot from one CDR repository into another,
//! dedup-aware: only chunks the destination lacks cross the wire, and
//! re-running is a no-op because snapshots are matched on a
//! content fingerprint rather than a local id.
//!
//! This is the 3-2-1 verb. `Repository::replicate_to` has been in
//! `freally-chunk` since 50h shipped and was reachable only from a
//! smoke test; an unattended host had no way to drive it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use freally_chunk::Repository;

use crate::ExitCode;
use crate::cli::{GlobalArgs, ReplicateArgs};
use crate::output::OutputWriter;

fn open(role: &str, root: &Path) -> Result<Repository, String> {
    Repository::open(root).map_err(|e| format!("open {role} repository at {}: {e}", root.display()))
}

/// Resolve a repository path, defaulting to this install's own.
///
/// Mirrors `freally repo` / `freally key` through the shared
/// [`super::repo_root`], so a portable install replicates *its*
/// repository rather than the host's.
fn resolve(explicit: Option<PathBuf>) -> Result<PathBuf, String> {
    match explicit {
        Some(p) => Ok(p),
        None => super::repo_root(),
    }
}

/// Whether two paths name the same repository.
///
/// A plain `==` is not enough: the source resolves to an absolute path
/// while the destination is whatever the caller typed, so
/// `freally replicate ./FreallyData/chunks` from inside a portable
/// install compared unequal and fell through to redb's "Database
/// already open" — a safe failure, but an opaque one.
///
/// `canonicalize` needs the path to exist, which the destination may
/// not; fall back to `absolute` (lexical, no I/O) and finally to the
/// path as given.
fn same_repo(a: &Path, b: &Path) -> bool {
    fn norm(p: &Path) -> PathBuf {
        std::fs::canonicalize(p)
            .or_else(|_| std::path::absolute(p))
            .unwrap_or_else(|_| p.to_path_buf())
    }
    norm(a) == norm(b)
}

pub(crate) async fn run(
    _global: &GlobalArgs,
    args: ReplicateArgs,
    writer: Arc<OutputWriter>,
) -> ExitCode {
    let src_root = match resolve(args.src.clone()) {
        Ok(p) => p,
        Err(m) => {
            writer.error(&m, ExitCode::GenericError.as_u8());
            return ExitCode::GenericError;
        }
    };
    if same_repo(&src_root, &args.dst) {
        // Replicating onto itself would take the same gc lease twice
        // and is never what the caller meant.
        writer.error(
            "source and destination are the same repository",
            ExitCode::ConfigInvalid.as_u8(),
        );
        return ExitCode::ConfigInvalid;
    }

    let src = match open("source", &src_root) {
        Ok(r) => r,
        Err(m) => {
            writer.error(&m, ExitCode::GenericError.as_u8());
            return ExitCode::GenericError;
        }
    };
    let dst = match open("destination", &args.dst) {
        Ok(r) => r,
        Err(m) => {
            writer.error(&m, ExitCode::GenericError.as_u8());
            return ExitCode::GenericError;
        }
    };

    let _ = writer.human(&format!(
        "replicating {} → {}",
        src_root.display(),
        args.dst.display()
    ));

    // `replicate_to` is synchronous and holds the destination's gc
    // lease for the whole pass; keep it off the async worker.
    let report = match tokio::task::spawn_blocking(move || src.replicate_to(&dst)).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            writer.error(&format!("replicate: {e}"), ExitCode::GenericError.as_u8());
            return ExitCode::GenericError;
        }
        Err(e) => {
            writer.error(
                &format!("replicate task: {e}"),
                ExitCode::GenericError.as_u8(),
            );
            return ExitCode::GenericError;
        }
    };

    let _ = writer.human(&format!(
        "snapshots: {} copied, {} already present",
        report.snapshots_copied, report.snapshots_skipped
    ));
    let _ = writer.human(&format!(
        "chunks: {} transferred, {} deduped away ({} bytes)",
        report.chunks_copied, report.chunks_present, report.bytes_copied
    ));
    ExitCode::Success
}
