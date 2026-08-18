//! FFM-M23 — source-stability guard, end-to-end against real files.
//!
//! ## Why there is no "race a writer against a live copy" test here
//!
//! Detecting a mid-read rewrite is inherently timing-dependent: the
//! guard compares a `stat` taken before the read pass with one taken
//! after. A test that spawns a writer and hopes it lands inside that
//! window is exactly the kind of flaky suite this repo already pays for
//! elsewhere. So the split is:
//!
//! - **Detection** is proven deterministically at the `SourceStamp`
//!   level, mutating a real file between two real stats.
//! - **Policy** is proven exhaustively by the pure `evaluate` unit
//!   tests in `src/stability.rs`.
//! - **Wiring** is proven here by asserting the guard is silent on a
//!   stable source under every policy — the property that would break
//!   users if it regressed, since a false positive marks every good
//!   copy as torn.

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use freally_core::stability::{SourceStability, SourceStamp, StabilityVerdict, evaluate};
use freally_core::{
    CopyControl, CopyError, CopyEvent, CopyOptions, CopyReport, FastCopyHook, FastCopyHookOutcome,
    JournalSink, MoveOptions, ResumePlan, copy_file, move_file,
};
use tokio::sync::mpsc;

async fn write(path: &Path, bytes: &[u8]) {
    tokio::fs::write(path, bytes).await.expect("write fixture");
}

/// Run one copy and return every event it emitted.
async fn copy_collecting(
    src: &Path,
    dst: &Path,
    policy: SourceStability,
) -> (Result<u64, String>, Vec<CopyEvent>) {
    let opts = CopyOptions {
        source_stability: policy,
        ..CopyOptions::default()
    };
    let (tx, mut rx) = mpsc::channel(256);
    let handle = tokio::spawn({
        let src = src.to_path_buf();
        let dst = dst.to_path_buf();
        async move { copy_file(&src, &dst, opts, CopyControl::new(), tx).await }
    });
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }
    let result = handle
        .await
        .expect("copy task joined")
        .map(|r| r.bytes)
        .map_err(|e| e.to_string());
    (result, events)
}

fn source_changed_events(events: &[CopyEvent]) -> Vec<(String, bool)> {
    events
        .iter()
        .filter_map(|e| match e {
            CopyEvent::SourceChanged {
                detail, recopying, ..
            } => Some((detail.clone(), *recopying)),
            _ => None,
        })
        .collect()
}

/// A fast-copy hook that performs the copy and then rewrites the
/// source, so the guard's "after" stamp always differs from its
/// "before" one.
///
/// This is what makes the torn-copy path testable at all: the real
/// race needs a writer to land inside the read window, which is
/// exactly the flaky shape the module docs above refuse to write. The
/// hook reproduces the *observable* condition deterministically —
/// source stamp changed across the copy — which is the only input the
/// guard actually consults.
#[derive(Debug)]
struct TearTheSourceHook;

impl FastCopyHook for TearTheSourceHook {
    fn try_copy<'a>(
        &'a self,
        src: PathBuf,
        dst: PathBuf,
        _opts: CopyOptions,
        _ctrl: CopyControl,
        _events: mpsc::Sender<CopyEvent>,
    ) -> Pin<Box<dyn Future<Output = Result<FastCopyHookOutcome, CopyError>> + Send + 'a>> {
        Box::pin(async move {
            let bytes = tokio::fs::read(&src).await.expect("read source");
            tokio::fs::write(&dst, &bytes).await.expect("write dest");
            // Now simulate the concurrent writer: the destination
            // holds the old generation, the source moves on.
            tokio::fs::write(&src, b"second generation, a different length")
                .await
                .expect("rewrite source");
            Ok(FastCopyHookOutcome::Done(CopyReport {
                src,
                dst,
                bytes: bytes.len() as u64,
                duration: Duration::ZERO,
                rate_bps: 0,
                source_changed: None,
            }))
        })
    }
}

/// A stand-in for the real journal: `Restart` until a file has been
/// finished, `AlreadyComplete` afterwards — exactly the state the real
/// journal is in on a second attempt, because the first attempt called
/// `finish_file`.
///
/// Optionally rewrites the source **from inside `finish_file`**. That
/// hook fires after the copy loop has read every byte but before
/// `copy_file` takes its "after" stamp, which makes a torn source
/// reproducible on the real dense copy path without racing a
/// background writer. (Rust opens files with `FILE_SHARE_WRITE` on
/// Windows, so rewriting a source the engine still holds open is
/// allowed.)
#[derive(Debug, Default)]
struct FakeJournal {
    resume_plans_served: AtomicU64,
    final_hash: std::sync::Mutex<Option<[u8; 32]>>,
    /// `(path, replacement)` — written once, the first time a file is
    /// finished.
    tear_on_finish: Option<(PathBuf, &'static [u8])>,
}

impl JournalSink for FakeJournal {
    fn checkpoint(&self, _: u64, _: &Path, _: u64, _: u64, _: [u8; 32]) {}
    fn finish_file(&self, _: u64, final_hash: [u8; 32]) {
        let mut slot = self.final_hash.lock().unwrap();
        let first_time = slot.is_none();
        *slot = Some(final_hash);
        if first_time && let Some((path, replacement)) = &self.tear_on_finish {
            std::fs::write(path, replacement).expect("rewrite source mid-copy");
        }
    }
    fn resume_plan(&self, _: u64) -> ResumePlan {
        self.resume_plans_served.fetch_add(1, Ordering::Relaxed);
        match *self.final_hash.lock().unwrap() {
            Some(final_hash) => ResumePlan::AlreadyComplete { final_hash },
            None => ResumePlan::Restart,
        }
    }
    fn finish_job_succeeded(&self) {}
    fn finish_job_failed(&self) {}
    fn finish_job_cancelled(&self) {}
}

/// **The regression this whole fix exists for**, at the level every
/// source-deleting caller actually consults.
///
/// `Warn` is the default policy. Before the fix, `copy_file` detected
/// the tear, emitted `SourceChanged`, and then returned a plain `Ok`
/// carrying no trace of the verdict — so `move_file`'s slow path
/// unlinked the source immediately afterwards, `move_tree` deleted the
/// whole source tree, and the GUI runner's move-to-trash path trashed
/// the source. Nothing between the event and the deletion consulted
/// anything. The user's only coherent copy of the file was destroyed
/// and the survivor was internally inconsistent.
///
/// The fix is that the verdict rides out in `CopyReport`, and this is
/// the test that proves it does. A journal is attached because the GUI
/// runner attaches one to every job, and the original suite used
/// `CopyOptions::default()` (no journal) — that gap is why this
/// shipped.
#[tokio::test]
async fn a_torn_copy_under_warn_carries_the_verdict_out_in_its_report() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("busy.log");
    let dst = dir.path().join("copy.log");
    write(&src, b"first generation").await;

    let journal = Arc::new(FakeJournal::default());
    let opts = CopyOptions {
        source_stability: SourceStability::Warn,
        fast_copy_hook: Some(Arc::new(TearTheSourceHook)),
        journal: Some(journal.clone()),
        ..CopyOptions::default()
    };

    let (tx, mut rx) = mpsc::channel(256);
    let handle = tokio::spawn({
        let src = src.clone();
        let dst = dst.clone();
        async move { copy_file(&src, &dst, opts, CopyControl::new(), tx).await }
    });
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }
    let report = handle
        .await
        .expect("copy task joined")
        .expect("Warn keeps the bytes, so the copy itself reports Ok");

    assert!(
        report.source_changed.is_some(),
        "THE data-loss regression: the verdict must reach the caller in \
         the report, not only as a fire-and-forget event — every path \
         that deletes a source after a successful copy reads this field",
    );
    assert_eq!(
        source_changed_events(&events)
            .iter()
            .filter(|(_, recopying)| !recopying)
            .count(),
        1,
        "exactly one terminal SourceChanged event must fire",
    );
}

/// A hook that tears the source and then declines the copy, so the real
/// copy path still runs.
///
/// [`TearTheSourceHook`] returns `Done` and short-circuits
/// `copy_file_once` before it emits any terminal event, which makes it
/// useless for asserting anything about event *order*. This one rewrites
/// the source and then answers `NotSupported`, so the engine falls
/// through to its normal read/write path: the guard's "before" stamp was
/// taken ahead of the rewrite, the "after" stamp sees the new generation,
/// and the copy emits the terminal events it normally would.
#[derive(Debug)]
struct TearThenDeclineHook;

impl FastCopyHook for TearThenDeclineHook {
    fn try_copy<'a>(
        &'a self,
        src: PathBuf,
        _dst: PathBuf,
        _opts: CopyOptions,
        _ctrl: CopyControl,
        _events: mpsc::Sender<CopyEvent>,
    ) -> Pin<Box<dyn Future<Output = Result<FastCopyHookOutcome, CopyError>> + Send + 'a>> {
        Box::pin(async move {
            tokio::fs::write(&src, b"second generation, a different length")
                .await
                .expect("rewrite source");
            Ok(FastCopyHookOutcome::NotSupported)
        })
    }
}

/// The torn verdict must reach the channel *before* the `Completed` that
/// closes the same file out.
///
/// This ordering is load-bearing, not cosmetic. `copy_file_once` used to
/// emit `Completed` itself and the guard ran afterwards, so the sequence
/// was always `Completed(f)` then `SourceChanged(f)`. Every consumer that
/// reads the verdict while handling `Completed` therefore saw nothing for
/// the file that actually tore, and then applied that leftover verdict to
/// the *next* file to finish: the torn file was recorded as a clean `ok`
/// and an innocent one was flagged in its place.
#[tokio::test]
async fn the_torn_verdict_is_emitted_before_the_completed_it_describes() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("busy.log");
    let dst = dir.path().join("copy.log");
    write(&src, b"first generation").await;

    let opts = CopyOptions {
        source_stability: SourceStability::Warn,
        fast_copy_hook: Some(Arc::new(TearThenDeclineHook)),
        ..CopyOptions::default()
    };

    let (tx, mut rx) = mpsc::channel(256);
    let handle = tokio::spawn({
        let src = src.clone();
        let dst = dst.clone();
        async move { copy_file(&src, &dst, opts, CopyControl::new(), tx).await }
    });
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }
    let report = handle
        .await
        .expect("copy task joined")
        .expect("Warn keeps the bytes, so the copy itself reports Ok");
    assert!(
        report.source_changed.is_some(),
        "the fixture must actually tear, or this proves nothing",
    );

    let changed_at = events
        .iter()
        .position(|e| {
            matches!(
                e,
                CopyEvent::SourceChanged {
                    recopying: false,
                    ..
                }
            )
        })
        .expect("the guard must emit a terminal SourceChanged");
    let completed_at = events
        .iter()
        .position(|e| matches!(e, CopyEvent::Completed { .. }))
        .expect("a Warn copy still completes, so the terminal event must be re-emitted");

    assert!(
        changed_at < completed_at,
        "SourceChanged (index {changed_at}) must precede Completed (index {completed_at}); \
         a consumer that reads the verdict as it handles Completed would \
         otherwise attribute this tear to whichever file finishes next",
    );
}

/// Exactly one terminal `Completed` — the guard defers the engine's and
/// then re-sends it, and sending both would double-count every file.
#[tokio::test]
async fn the_deferred_completed_is_emitted_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("busy.log");
    let dst = dir.path().join("copy.log");
    write(&src, b"first generation").await;

    let opts = CopyOptions {
        source_stability: SourceStability::Warn,
        fast_copy_hook: Some(Arc::new(TearThenDeclineHook)),
        ..CopyOptions::default()
    };
    let (tx, mut rx) = mpsc::channel(256);
    let handle = tokio::spawn({
        let src = src.clone();
        let dst = dst.clone();
        async move { copy_file(&src, &dst, opts, CopyControl::new(), tx).await }
    });
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }
    handle
        .await
        .expect("copy task joined")
        .expect("Warn keeps the bytes");

    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, CopyEvent::Completed { .. }))
            .count(),
        1,
        "the terminal event must be deferred, not duplicated",
    );
}

/// A clean copy under an active guard still gets its terminal event.
///
/// The deferral only fires when the guard is on, so this is the case that
/// would silently lose `Completed` for every ordinary file if the
/// re-emission were ever dropped from the `Stable` arm.
#[tokio::test]
async fn a_stable_copy_under_an_active_guard_still_completes() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("calm.log");
    let dst = dir.path().join("copy.log");
    write(&src, b"nobody touches this").await;

    let (result, events) = copy_collecting(&src, &dst, SourceStability::Warn).await;
    assert!(result.is_ok(), "a stable copy must succeed");
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, CopyEvent::Completed { .. }))
            .count(),
        1,
        "the guard must not swallow Completed on the happy path",
    );
    assert!(
        source_changed_events(&events).is_empty(),
        "a stable source must not report a change",
    );
}

/// The same regression end-to-end through `move_file`.
///
/// `move_file` tries an atomic `rename` first and only falls back to
/// copy-then-delete on a genuine `CrossesDevices` error, so this can
/// only run where two filesystems are actually available. There is no
/// portable way to force the fallback — which is part of why the bug
/// went unnoticed. Where no cross-device pair exists the test skips
/// loudly rather than passing vacuously; the report-level test above
/// is the portable guard.
#[tokio::test]
async fn a_cross_device_move_whose_source_tore_keeps_the_source() {
    let Some((src_dir, dst_dir)) = cross_device_pair() else {
        eprintln!(
            "SKIP a_cross_device_move_whose_source_tore_keeps_the_source: \
             no cross-device pair on this host"
        );
        return;
    };
    let src = src_dir.join("busy.log");
    let dst = dst_dir.join("moved.log");
    write(&src, b"first generation").await;

    let opts = MoveOptions {
        copy: CopyOptions {
            source_stability: SourceStability::Warn,
            fast_copy_hook: Some(Arc::new(TearTheSourceHook)),
            journal: Some(Arc::new(FakeJournal::default())),
            ..CopyOptions::default()
        },
        ..MoveOptions::default()
    };

    let (tx, mut rx) = mpsc::channel(256);
    let handle = tokio::spawn({
        let src = src.clone();
        let dst = dst.clone();
        async move { move_file(&src, &dst, opts, CopyControl::new(), tx).await }
    });
    while rx.recv().await.is_some() {}
    let report = handle
        .await
        .expect("move task joined")
        .expect("Warn keeps the bytes, so the move itself reports Ok");

    assert!(
        src.exists(),
        "a move whose source tore must never unlink the source — it is \
         the only coherent copy left",
    );
    assert!(report.source_changed.is_some());
    let _ = tokio::fs::remove_file(&src).await;
    let _ = tokio::fs::remove_file(&dst).await;
}

/// Two directories on different filesystems, if this host has any.
///
/// On Linux `/dev/shm` is a tmpfs and the system temp dir is normally
/// on the root filesystem, which is a real `EXDEV` pair. Elsewhere we
/// have no reliable way to find one.
fn cross_device_pair() -> Option<(PathBuf, PathBuf)> {
    if cfg!(target_os = "linux") {
        let shm = PathBuf::from("/dev/shm");
        if shm.is_dir() {
            let here = std::env::temp_dir();
            let a = here.join("freally-stability-src");
            let b = shm.join("freally-stability-dst");
            std::fs::create_dir_all(&a).ok()?;
            std::fs::create_dir_all(&b).ok()?;
            return Some((a, b));
        }
    }
    None
}

/// `Recopy` must actually re-read the source.
///
/// Before the fix the retry re-entered the copy with the journal still
/// attached, so `decide_resume` saw the `AlreadyComplete` record the
/// first attempt had *just* written, confirmed the destination's
/// length and hash against it, and returned a synthetic success
/// **without ever opening the source**. The user asked to copy the
/// file again and got the original torn bytes reported as
/// successfully recopied.
///
/// The tear here keeps the length identical on purpose. A source whose
/// *length* changed already forced a fresh start via
/// `DstLengthMismatch`, so a differing-length fixture would pass
/// against the unfixed code and prove nothing — the same-length case
/// is the one that was actually broken.
#[tokio::test]
async fn recopy_actually_recopies_instead_of_trusting_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("busy.log");
    let dst = dir.path().join("copy.log");
    write(&src, b"first generation").await;

    let journal = Arc::new(FakeJournal {
        tear_on_finish: Some((src.clone(), b"SECOND GENERATION")),
        ..FakeJournal::default()
    });
    let opts = CopyOptions {
        source_stability: SourceStability::Recopy,
        journal: Some(journal.clone()),
        ..CopyOptions::default()
    };

    let (tx, mut rx) = mpsc::channel(256);
    let handle = tokio::spawn({
        let src = src.clone();
        let dst = dst.clone();
        async move { copy_file(&src, &dst, opts, CopyControl::new(), tx).await }
    });
    while rx.recv().await.is_some() {}
    handle
        .await
        .expect("copy task joined")
        .expect("Recopy keeps the second attempt's bytes");

    // The destination must hold the *second* generation. Against the
    // unfixed engine this is still `first generation`: the retry
    // short-circuited on the journal record and copied nothing.
    assert_eq!(
        tokio::fs::read(&dst).await.unwrap(),
        b"SECOND GENERATION",
        "the retry must actually re-read the source, not trust the \
         journal's just-written AlreadyComplete record",
    );
    // And it must reach that state by never consulting the journal at
    // all. The first attempt doesn't (no destination exists yet, so
    // `decide_resume` returns early), and the retry doesn't because it
    // detaches the journal — so any call at all means the retry
    // consulted the `AlreadyComplete` record and skipped the copy.
    assert_eq!(
        journal.resume_plans_served.load(Ordering::Relaxed),
        0,
        "the retry must not consult the journal — its AlreadyComplete \
         record would skip the copy entirely",
    );
}

/// `Fail` must leave nothing behind. Before the fix the arm returned a
/// bare `Err`, so a full-size, correctly-dated, torn file stayed at the
/// destination — which the next run under skip-existing or newer-only
/// would treat as already copied — and no `CopyEvent::Failed` was ever
/// delivered, unlike every other engine failure.
#[tokio::test]
async fn fail_removes_the_torn_destination_and_emits_failed() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("busy.log");
    let dst = dir.path().join("copy.log");
    write(&src, b"first generation").await;

    let opts = CopyOptions {
        source_stability: SourceStability::Fail,
        fast_copy_hook: Some(Arc::new(TearTheSourceHook)),
        ..CopyOptions::default()
    };

    let (tx, mut rx) = mpsc::channel(256);
    let handle = tokio::spawn({
        let src = src.clone();
        let dst = dst.clone();
        async move { copy_file(&src, &dst, opts, CopyControl::new(), tx).await }
    });
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }
    handle
        .await
        .expect("copy task joined")
        .expect_err("Fail must fail the item");

    assert!(
        !dst.exists(),
        "a torn destination must not survive — the next skip-existing \
         run would treat it as already copied",
    );
    assert!(
        events.iter().any(|e| matches!(e, CopyEvent::Failed { .. })),
        "the single-file UI path only learns of a failure from this event",
    );
}

#[tokio::test]
async fn a_stable_source_never_reports_a_change_under_any_policy() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    write(&src, &vec![7u8; 64 * 1024]).await;

    for (i, policy) in [
        SourceStability::Off,
        SourceStability::Warn,
        SourceStability::Recopy,
        SourceStability::Fail,
    ]
    .into_iter()
    .enumerate()
    {
        let dst = dir.path().join(format!("dst{i}.bin"));
        let (result, events) = copy_collecting(&src, &dst, policy).await;
        assert_eq!(result, Ok(64 * 1024), "{policy:?} must copy successfully");
        assert!(
            source_changed_events(&events).is_empty(),
            "{policy:?} raised a false positive on an untouched source",
        );
        assert_eq!(
            tokio::fs::read(&dst).await.unwrap(),
            vec![7u8; 64 * 1024],
            "{policy:?} must still produce byte-exact output",
        );
    }
}

#[tokio::test]
async fn the_guard_costs_nothing_observable_when_off() {
    // The `Off` policy must skip both stats and behave exactly as the
    // pre-FFM-M23 engine did: same bytes, same events, no extra work
    // visible to the caller.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    write(&src, b"hello portable world").await;

    let dst = dir.path().join("dst.bin");
    let (result, events) = copy_collecting(&src, &dst, SourceStability::Off).await;
    assert_eq!(result, Ok(20));
    assert!(source_changed_events(&events).is_empty());
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CopyEvent::Completed { .. })),
        "the normal completion event must still fire",
    );
}

#[tokio::test]
async fn a_real_rewrite_between_two_real_stats_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("busy.log");
    write(&src, b"first generation").await;

    let before = SourceStamp::of(&src, true).await.expect("stat before");
    write(&src, b"second generation, longer than the first").await;
    let after = SourceStamp::of(&src, true).await.expect("stat after");

    assert_ne!(before, after, "a rewrite must change the stamp");
    assert_eq!(
        evaluate(Some(before), Some(after), SourceStability::Fail),
        StabilityVerdict::Fail,
    );
    assert!(
        before.describe_change(&after).contains("size 16 → 40"),
        "detail should name the size change: {}",
        before.describe_change(&after),
    );
}

#[tokio::test]
async fn an_in_place_rewrite_of_identical_length_is_still_detected() {
    // Same length, so a size-only guard would miss it. mtime (or, on
    // Windows, mtime plus creation time) is what catches it.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("fixed-width.log");
    write(&src, b"AAAAAAAAAAAAAAAA").await;
    let before = SourceStamp::of(&src, true).await.expect("stat before");

    // Filesystem mtime granularity can be as coarse as 1-2 s; wait past
    // it so this asserts the guard's logic rather than the host's clock
    // resolution.
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    write(&src, b"BBBBBBBBBBBBBBBB").await;
    let after = SourceStamp::of(&src, true).await.expect("stat after");

    assert_eq!(before.len, after.len, "the fixture must keep the length");
    assert_ne!(before, after, "the stamp must still differ");
    assert_eq!(
        evaluate(Some(before), Some(after), SourceStability::Warn),
        StabilityVerdict::Warn,
    );
}

#[tokio::test]
async fn a_vanished_source_is_left_to_the_normal_io_error_path() {
    // `SourceStamp::of` returns None rather than inventing a change, so
    // a deleted source surfaces as the engine's own NotFound instead of
    // a confusing "source changed" verdict.
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("gone.bin");
    assert!(SourceStamp::of(&missing, true).await.is_none());

    let dst = dir.path().join("dst.bin");
    let (result, _) = copy_collecting(&missing, &dst, SourceStability::Fail).await;
    let err = result.expect_err("copying a missing source must fail");
    assert!(
        !err.contains("source changed"),
        "a missing source must not be reported as a mid-read change: {err}",
    );
}
