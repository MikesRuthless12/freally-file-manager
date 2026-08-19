//! Subcommand modules. Each one exposes a single `pub(crate) async fn
//! run(...)` that returns the resolved `ExitCode`.

use std::path::PathBuf;
use std::sync::Arc;

use freally_chunk::Repository;

use crate::ExitCode;
use crate::output::OutputWriter;

/// Resolve the repository root for this install.
///
/// Mirrors the GUI's selection (FFM-M21): a portable install keeps its
/// chunk store beside the binary, so the CLI must look in the same
/// place or it would silently operate on the host's repository instead.
///
/// Shared deliberately. This rule was copied into four subcommands, and
/// a copy that drifts does not fail to compile — it quietly reads the
/// wrong repository.
pub(crate) fn repo_root() -> Result<PathBuf, String> {
    match freally_settings::portable::portable_root() {
        Some(root) => Ok(root.join("chunks")),
        None => freally_chunk::default_chunk_store_path()
            .map_err(|e| format!("cannot resolve the repository path: {e}")),
    }
}

/// Open this install's repository, at the root [`repo_root`] picks.
pub(crate) fn open_repo() -> Result<Repository, String> {
    let root = repo_root()?;
    Repository::open(&root).map_err(|e| format!("open repository at {}: {e}", root.display()))
}

/// Report `message` on the error channel and resolve to
/// [`ExitCode::GenericError`].
pub(crate) fn fail(writer: &Arc<OutputWriter>, message: String) -> ExitCode {
    writer.error(&message, ExitCode::GenericError.as_u8());
    ExitCode::GenericError
}

pub(crate) mod audit;
pub(crate) mod completions;
pub(crate) mod config;
pub(crate) mod copy;
pub(crate) mod history;
pub(crate) mod key;
pub(crate) mod migrate;
pub(crate) mod mount;
pub(crate) mod plan;
pub(crate) mod provenance;
pub(crate) mod remote;
pub(crate) mod replicate;
pub(crate) mod repo;
pub(crate) mod schedule;
pub(crate) mod serve;
pub(crate) mod shred;
pub(crate) mod stack;
pub(crate) mod sync;
pub(crate) mod verify;
pub(crate) mod version;
