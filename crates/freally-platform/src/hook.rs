//! `FastCopyHook` adapter — the bridge into [`freally_core`].
//!
//! Drop a [`PlatformFastCopyHook`] into [`freally_core::CopyOptions::fast_copy_hook`]
//! and every per-file `copy_file` (and therefore every per-file leaf
//! of `copy_tree`) routes through [`crate::fast_copy`] before the
//! engine's own async loop runs.

use std::path::PathBuf;
use std::pin::Pin;

use freally_core::{
    CopyControl, CopyError, CopyEvent, CopyOptions, CopyReport, FastCopyHook, FastCopyHookOutcome,
};
use tokio::sync::mpsc;

use crate::dispatcher::fast_copy;

/// `FastCopyHook` implementation backed by [`crate::fast_copy`].
///
/// Stateless and cheap to clone — wrap once in `Arc::new` at app
/// startup and reuse forever.
#[derive(Debug, Default, Clone, Copy)]
pub struct PlatformFastCopyHook;

impl FastCopyHook for PlatformFastCopyHook {
    fn try_copy<'a>(
        &'a self,
        src: PathBuf,
        dst: PathBuf,
        opts: CopyOptions,
        ctrl: CopyControl,
        events: mpsc::Sender<CopyEvent>,
    ) -> Pin<
        Box<dyn std::future::Future<Output = Result<FastCopyHookOutcome, CopyError>> + Send + 'a>,
    > {
        Box::pin(async move {
            // `Ok(None)` means no fast path applied and nothing was
            // copied — report `NotSupported` so the engine runs its own
            // async loop. That loop finalizes the journal, the
            // provenance record and `CopyEvent::Completed` behind the
            // source-stability verdict; a hook that copied the bytes
            // itself and reported `Done` would bypass all of that.
            let Some(outcome) = fast_copy(&src, &dst, opts, ctrl, events).await? else {
                return Ok(FastCopyHookOutcome::NotSupported);
            };
            Ok(FastCopyHookOutcome::Done(CopyReport {
                src,
                dst,
                bytes: outcome.bytes,
                duration: outcome.duration,
                rate_bps: outcome.rate_bps,
                // `copy_file` owns the stability verdict and stamps it
                // onto whatever report the hook hands back.
                source_changed: None,
            }))
        })
    }
}
