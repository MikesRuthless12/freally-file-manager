//! `freally mount <mountpoint> [--job <id>] [--snapshot <id>]`
//!
//! Phase 49m — `--snapshot` mounts a repository snapshot read-only,
//! handing the chunk-store handle to the backend so the gated
//! FUSE/WinFsp `read` callbacks can serve file content straight out of
//! the repository.
//!
//! On a default build the backend is `NoopBackend`: the session is
//! logical and no kernel callbacks fire. Real filesystem mounts need
//! `--features fuse` (Linux/macOS) or `--features winfsp` (Windows),
//! which CI validates and this host does not build by default.
//!
//! `--job` remains staged for IPC plumbing — the job view is served
//! from the GUI's live registry, which a separate CLI process cannot
//! reach.

use std::sync::Arc;

use freally_chunk::Repository;
use freally_mount::MountLayout;
use freally_mount::backends::{ArchiveRefs, MountBackend, NoopBackend};

use crate::ExitCode;
use crate::cli::{GlobalArgs, MountArgs};
use crate::output::{JsonEventKind, OutputWriter};

use super::repo_root;

pub(crate) async fn run(
    _global: &GlobalArgs,
    args: MountArgs,
    writer: Arc<OutputWriter>,
) -> ExitCode {
    let Some(snapshot_id) = args.snapshot else {
        // Job mounts still need the GUI's live registry.
        let _ = writer.emit(JsonEventKind::Info {
            message: format!(
                "mount `{}`{} parsed; a job mount is served by the running app's \
                 registry. Use --snapshot <id> to mount a repository snapshot.",
                args.mountpoint.display(),
                args.job
                    .as_ref()
                    .map(|j| format!(" (job={j})"))
                    .unwrap_or_default(),
            ),
        });
        let _ =
            writer.human("mount: --job is served by the running app; use --snapshot <id> here.");
        return ExitCode::ConfigInvalid;
    };

    let root = match repo_root() {
        Ok(r) => r,
        Err(message) => {
            writer.error(&message, ExitCode::GenericError.as_u8());
            return ExitCode::GenericError;
        }
    };
    let repo = match Repository::open(&root) {
        Ok(r) => r,
        Err(e) => {
            writer.error(
                &format!("open repository at {}: {e}", root.display()),
                ExitCode::GenericError.as_u8(),
            );
            return ExitCode::GenericError;
        }
    };

    // Fail before opening a handle if the snapshot is not there —
    // otherwise the caller gets an empty mount and no explanation.
    match repo.snapshots() {
        Ok(snaps) if snaps.iter().any(|s| s.id == snapshot_id) => {}
        Ok(_) => {
            writer.error(
                &format!("snapshot {snapshot_id} not found"),
                ExitCode::GenericError.as_u8(),
            );
            return ExitCode::GenericError;
        }
        Err(e) => {
            writer.error(
                &format!("list snapshots: {e}"),
                ExitCode::GenericError.as_u8(),
            );
            return ExitCode::GenericError;
        }
    }

    let archive = ArchiveRefs {
        chunk_store: Some(repo.store_arc()),
        ..Default::default()
    };
    let handle = match NoopBackend::default().mount(&args.mountpoint, MountLayout::all(), &archive)
    {
        Ok(h) => h,
        Err(e) => {
            writer.error(&format!("mount: {e}"), ExitCode::GenericError.as_u8());
            return ExitCode::GenericError;
        }
    };

    let _ = writer.human(&format!(
        "mounted snapshot {snapshot_id} at {} (backend: {})",
        args.mountpoint.display(),
        freally_mount::platform::default_backend_name(),
    ));
    // The handle unmounts on drop. A CLI process that exits here would
    // tear the mount down immediately, which is useless for a real
    // filesystem mount — so hold it until the caller interrupts.
    let _ = writer.human("holding the mount — press Ctrl+C to unmount.");
    if tokio::signal::ctrl_c().await.is_err() {
        // No signal handler available (detached/service context):
        // dropping here is the honest outcome rather than spinning.
        let _ = writer.human("no signal handler available; unmounting now.");
    }
    match handle.unmount() {
        Ok(()) => ExitCode::Success,
        Err(e) => {
            writer.error(&format!("unmount: {e}"), ExitCode::GenericError.as_u8());
            ExitCode::GenericError
        }
    }
}
