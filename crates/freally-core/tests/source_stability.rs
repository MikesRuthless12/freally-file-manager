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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use freally_core::stability::{SourceStability, SourceStamp, StabilityVerdict, evaluate};
use freally_core::{
    CopyControl, CopyError, CopyEvent, CopyOptions, CopyReport, FastCopyHook, FastCopyHookOutcome,
    JournalSink, MoveOptions, ResumePlan, ShapeSink, copy_file, move_file,
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
/// journal is in on a second attempt if the first one called
/// `finish_file`.
///
/// Keeps the last checkpoint as well as the final hash, so
/// `resume_plan` can hand back a `Resume` the way the real one does —
/// which is what makes resume laundering observable here.
#[derive(Debug, Default)]
struct FakeJournal {
    resume_plans_served: AtomicU64,
    final_hash: std::sync::Mutex<Option<[u8; 32]>>,
    /// `(bytes_done, hash_so_far)` from the most recent checkpoint.
    checkpoint: std::sync::Mutex<Option<(u64, [u8; 32])>>,
    invalidations: AtomicU64,
}

impl FakeJournal {
    /// Did anything record this file as complete?
    fn recorded_complete(&self) -> bool {
        self.final_hash.lock().unwrap().is_some()
    }

    fn invalidations(&self) -> u64 {
        self.invalidations.load(Ordering::SeqCst)
    }

    /// What the *next* run's resume probe would be told, without
    /// bumping the served counter.
    fn peek_plan(&self) -> ResumePlan {
        match *self.final_hash.lock().unwrap() {
            Some(final_hash) => ResumePlan::AlreadyComplete { final_hash },
            None => match *self.checkpoint.lock().unwrap() {
                Some((offset, src_hash_at_offset)) if offset > 0 => ResumePlan::Resume {
                    offset,
                    src_hash_at_offset,
                },
                _ => ResumePlan::Restart,
            },
        }
    }
}

impl JournalSink for FakeJournal {
    fn checkpoint(&self, _: u64, _: &Path, bytes_done: u64, _: u64, hash_so_far: [u8; 32]) {
        *self.checkpoint.lock().unwrap() = Some((bytes_done, hash_so_far));
    }
    fn finish_file(&self, _: u64, final_hash: [u8; 32]) {
        *self.final_hash.lock().unwrap() = Some(final_hash);
    }
    fn resume_plan(&self, _: u64) -> ResumePlan {
        self.resume_plans_served.fetch_add(1, Ordering::Relaxed);
        self.peek_plan()
    }
    fn invalidate_file(&self, _: u64) {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
        *self.final_hash.lock().unwrap() = None;
        *self.checkpoint.lock().unwrap() = None;
    }
    fn finish_job_succeeded(&self) {}
    fn finish_job_failed(&self) {}
    fn finish_job_cancelled(&self) {}
}

/// Rewrites the source the first time the copy loop asks for a
/// bandwidth permit.
///
/// `ShapeSink::permit` is called from inside the dense read loop after
/// every buffered read, which puts the rewrite squarely inside the
/// window the guard's two stamps bracket — the real-world shape of a
/// torn copy, made deterministic instead of raced. (Rust opens files
/// with `FILE_SHARE_WRITE` on Windows, so rewriting a source the
/// engine still holds open is allowed.)
///
/// This replaces an earlier hook that tore from inside
/// `JournalSink::finish_file`. That worked only while the journal was
/// finalized *before* the "after" stamp — which was the §A data-loss
/// bug. It cannot be used to reproduce a tear any more, precisely
/// because the ordering it depended on is what got fixed.
///
/// Only the first permit tears, so a `Recopy` retry runs against a
/// source that has settled.
#[derive(Debug)]
struct TearOnFirstRead {
    src: PathBuf,
    replacement: &'static [u8],
    /// When set, only a read of exactly this many bytes tears. A tree
    /// copy shares one shaper across every file and gives no other clue
    /// which file is asking, so sizing the intended victim differently
    /// from its siblings is what makes "tear *this* file" deterministic
    /// instead of a race.
    when_read_len: Option<u64>,
    fired: AtomicBool,
}

impl TearOnFirstRead {
    fn new(src: &Path, replacement: &'static [u8]) -> Self {
        Self {
            src: src.to_path_buf(),
            replacement,
            when_read_len: None,
            fired: AtomicBool::new(false),
        }
    }

    /// As [`Self::new`], but only fires on a read of `len` bytes.
    fn on_read_len(src: &Path, replacement: &'static [u8], len: u64) -> Self {
        Self {
            when_read_len: Some(len),
            ..Self::new(src, replacement)
        }
    }
}

impl ShapeSink for TearOnFirstRead {
    fn permit<'a>(&'a self, bytes: u64) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            if self.when_read_len.is_some_and(|want| want != bytes) {
                return;
            }
            if !self.fired.swap(true, Ordering::SeqCst) {
                std::fs::write(&self.src, self.replacement).expect("rewrite source mid-copy");
            }
        })
    }
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

    let journal = Arc::new(FakeJournal::default());
    let opts = CopyOptions {
        source_stability: SourceStability::Recopy,
        journal: Some(journal.clone()),
        // Same length as the original on purpose — see the doc above.
        shape: Some(Arc::new(TearOnFirstRead::new(&src, b"SECOND GENERATIO"))),
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
        b"SECOND GENERATIO",
        "the retry must actually re-read the source, not trust the \
         journal's just-written AlreadyComplete record",
    );
    // And it must reach that state by never consulting the journal at
    // all. The first attempt doesn't (no destination exists yet, so
    // `decide_resume` returns early), and the retry doesn't because it
    // detaches the journal — so any call at all means the retry
    // consulted the record and skipped the copy.
    assert_eq!(
        journal.resume_plans_served.load(Ordering::Relaxed),
        0,
        "the retry must not consult the journal — a resumable record \
         would skip or short-copy the retry",
    );
    // Belt and braces on the §A fix: the discarded first attempt must
    // not have recorded its torn bytes as a finished file either.
    assert!(
        !journal.recorded_complete(),
        "the abandoned first attempt must not finalize the journal",
    );
}

// ---------------------------------------------------------------
// §A — finalization must not happen before the stability verdict.
//
// Three finalization steps used to run inside the copy proper, before
// the wrapper had taken its "after" stamp: the journal row, the
// provenance record, and `CopyEvent::Completed`. Each is one of the
// three symptoms below. All four tests here run the **dense** copy
// path with a real journal attached, because that is the shape the GUI
// runner uses for every job and the shape the original suite never
// covered.
// ---------------------------------------------------------------

/// Symptom 1 — the wire order.
///
/// `Completed` used to be emitted from inside the copy, i.e. *before*
/// the guard had even looked at the source, so every consumer that
/// pairs a terminal `SourceChanged` with the `Completed` that follows
/// it saw the two in the opposite order. In the GUI runner that made
/// the torn-file lookup always miss: a torn file was written to
/// history as a plain `ok` with no error code, and the audit sink
/// recorded a clean `FileCopied`.
#[tokio::test]
async fn source_changed_reaches_the_channel_before_the_matching_completed() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("busy.log");
    let dst = dir.path().join("copy.log");
    write(&src, b"first generation").await;

    let opts = CopyOptions {
        source_stability: SourceStability::Warn,
        journal: Some(Arc::new(FakeJournal::default())),
        shape: Some(Arc::new(TearOnFirstRead::new(&src, b"SECOND GENERATION"))),
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
        .expect("the guard must report the tear");
    let completed_at = events
        .iter()
        .position(|e| matches!(e, CopyEvent::Completed { .. }))
        .expect("the file still completes under Warn");

    assert!(
        changed_at < completed_at,
        "SourceChanged must reach the channel before the Completed it \
         belongs to; reversed, every consumer that pairs them records a \
         torn file as clean (events: {events:#?})",
    );
}

/// Symptom 1, the tree-mode half — `Completed` must say which file it
/// is for.
///
/// It used to carry no `src` at all. In tree mode the engine runs
/// several files concurrently over one shared channel, so a consumer
/// could only guess "the most recently started file" — which meant a
/// stale verdict landed on whichever file's `Completed` arrived next:
/// a false positive on a clean file and a false negative on the torn
/// one.
#[tokio::test]
async fn every_completed_names_the_file_it_finished() {
    use freally_core::{TreeOptions, copy_tree};

    let dir = tempfile::tempdir().unwrap();
    let src_dir = dir.path().join("src");
    let dst_dir = dir.path().join("dst");
    tokio::fs::create_dir_all(&src_dir).await.unwrap();

    let mut expected: Vec<PathBuf> = Vec::new();
    for i in 0..12 {
        let f = src_dir.join(format!("file{i}.bin"));
        write(&f, &vec![i as u8; 4096]).await;
        expected.push(f);
    }
    expected.sort();

    let (tx, mut rx) = mpsc::channel(512);
    let handle = tokio::spawn({
        let src_dir = src_dir.clone();
        let dst_dir = dst_dir.clone();
        async move {
            copy_tree(
                &src_dir,
                &dst_dir,
                TreeOptions::default(),
                CopyControl::new(),
                tx,
            )
            .await
        }
    });
    let mut completed: Vec<PathBuf> = Vec::new();
    while let Some(ev) = rx.recv().await {
        if let CopyEvent::Completed { src, .. } = ev {
            completed.push(src);
        }
    }
    handle
        .await
        .expect("tree task joined")
        .expect("tree copies");
    completed.sort();

    assert_eq!(
        completed, expected,
        "each per-file Completed must name its own source; without that \
         a concurrent tree cannot attribute a verdict to a file at all",
    );
}

/// Symptom 2 — the journal must not launder a tear across a resume.
///
/// This is the irrecoverable one. `finish_file` used to run inside the
/// copy, so a torn destination was recorded as complete with a final
/// hash. On the next run `decide_resume` returned `AlreadyComplete`
/// and the engine handed back a clean report **without ever opening
/// the source** — and a `move` on the strength of that report unlinks
/// the source. It survives an app restart and is not
/// trash-recoverable.
#[tokio::test]
async fn a_torn_copy_is_never_recorded_complete_in_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("busy.log");
    let dst = dir.path().join("copy.log");
    write(&src, b"first generation").await;

    let journal = Arc::new(FakeJournal::default());
    let opts = CopyOptions {
        source_stability: SourceStability::Warn,
        journal: Some(journal.clone()),
        shape: Some(Arc::new(TearOnFirstRead::new(&src, b"SECOND GENERATION"))),
        ..CopyOptions::default()
    };

    let (tx, mut rx) = mpsc::channel(256);
    let handle = tokio::spawn({
        let src = src.clone();
        let dst = dst.clone();
        async move { copy_file(&src, &dst, opts, CopyControl::new(), tx).await }
    });
    while rx.recv().await.is_some() {}
    let report = handle
        .await
        .expect("copy task joined")
        .expect("Warn keeps the bytes");
    assert!(
        report.source_changed.is_some(),
        "the guard must see the tear"
    );

    assert!(
        !journal.recorded_complete(),
        "a torn destination must not be journalled as complete — the \
         next run would skip it and report success without reading the \
         source, and a move would then unlink the source",
    );
    // Declining to finalize is NOT enough on its own. The checkpoints
    // the first attempt left behind hash the torn read stream, and the
    // torn destination holds exactly those bytes — so next run the
    // prefix check matches, resume is accepted, the torn prefix
    // survives, and the file is then finalized and certified clean.
    // The row has to be actively invalidated.
    assert!(
        journal.invalidations() >= 1,
        "the torn file's journal row must be invalidated, not merely \
         left unfinished",
    );
    assert!(
        matches!(journal.peek_plan(), ResumePlan::Restart),
        "after a tear the next run must be told to restart the file, \
         never to resume onto the torn prefix (got {:?})",
        journal.peek_plan(),
    );

    // And prove the consequence, not just the state: a second run over
    // the same journal must actually re-copy rather than short-circuit.
    let opts2 = CopyOptions {
        source_stability: SourceStability::Warn,
        journal: Some(journal.clone()),
        ..CopyOptions::default()
    };
    let (tx2, mut rx2) = mpsc::channel(256);
    let handle2 = tokio::spawn({
        let src = src.clone();
        let dst = dst.clone();
        async move { copy_file(&src, &dst, opts2, CopyControl::new(), tx2).await }
    });
    while rx2.recv().await.is_some() {}
    let second = handle2
        .await
        .expect("second copy joined")
        .expect("second copy succeeds");

    assert!(
        second.source_changed.is_none(),
        "the source has settled, so the second run must be clean",
    );
    assert_eq!(
        tokio::fs::read(&dst).await.unwrap(),
        b"SECOND GENERATION",
        "the re-copy must replace the torn bytes with the settled source",
    );
}

/// Symptom 3 — provenance must not certify torn bytes.
///
/// `record_file` used to run inside the copy too, so the signed
/// manifest claimed a torn destination was a faithful copy of its
/// source. The sink has no way to flag an entry, so the only honest
/// outcome is that the file is absent from the manifest and a later
/// verify reports it missing rather than passing it as certified.
#[tokio::test]
async fn a_torn_copy_is_never_certified_in_the_provenance_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("busy.log");
    let dst = dir.path().join("copy.log");
    write(&src, b"first generation").await;

    let sink = Arc::new(RecordingProvenanceSink::default());
    let opts = CopyOptions {
        source_stability: SourceStability::Warn,
        journal: Some(Arc::new(FakeJournal::default())),
        provenance: Some(freally_core::ProvenancePolicy { sink: sink.clone() }),
        shape: Some(Arc::new(TearOnFirstRead::new(&src, b"SECOND GENERATION"))),
        ..CopyOptions::default()
    };

    let (tx, mut rx) = mpsc::channel(256);
    let handle = tokio::spawn({
        let src = src.clone();
        let dst = dst.clone();
        async move { copy_file(&src, &dst, opts, CopyControl::new(), tx).await }
    });
    while rx.recv().await.is_some() {}
    let report = handle
        .await
        .expect("copy task joined")
        .expect("Warn keeps the bytes");
    assert!(
        report.source_changed.is_some(),
        "the guard must see the tear"
    );

    assert_eq!(
        sink.recorded(),
        0,
        "a torn destination must not appear in the provenance manifest — \
         the manifest's whole claim is that the recorded digest matches \
         the source, and for a torn file it does not",
    );
}

/// The input to `move_tree`'s "keep the sources" guard.
///
/// That guard (`if report.source_changed > 0 { return Ok(report) }`)
/// had zero coverage, and it is only as good as the tally it reads. A
/// torn descendant has to propagate all the way from the per-file
/// `CopyReport` up into `TreeReport::source_changed`; if it does not,
/// the guard is dead code and `move_tree` unlinks the source of a torn
/// file.
///
/// Portable, unlike the end-to-end move test below — `copy_tree` has
/// no atomic-rename fast path to dodge.
#[tokio::test]
async fn copy_tree_tallies_a_torn_descendant_into_its_report() {
    use freally_core::{TreeOptions, copy_tree};

    let dir = tempfile::tempdir().unwrap();
    let src_dir = dir.path().join("src");
    let dst_dir = dir.path().join("dst");
    tokio::fs::create_dir_all(src_dir.join("nested"))
        .await
        .unwrap();

    // Sized apart so the shaper can pick its victim deterministically.
    let calm = src_dir.join("calm.bin");
    let busy = src_dir.join("nested").join("busy.log");
    write(&calm, &vec![3u8; 8192]).await;
    write(&busy, b"first generation").await;

    let opts = TreeOptions {
        file: CopyOptions {
            source_stability: SourceStability::Warn,
            shape: Some(Arc::new(TearOnFirstRead::on_read_len(
                &busy,
                b"SECOND GENERATION",
                b"first generation".len() as u64,
            ))),
            ..CopyOptions::default()
        },
        ..TreeOptions::default()
    };

    let (tx, mut rx) = mpsc::channel(512);
    let handle = tokio::spawn({
        let src_dir = src_dir.clone();
        let dst_dir = dst_dir.clone();
        async move { copy_tree(&src_dir, &dst_dir, opts, CopyControl::new(), tx).await }
    });
    while rx.recv().await.is_some() {}
    let report = handle
        .await
        .expect("tree task joined")
        .expect("Warn keeps the bytes, so the tree copy succeeds");

    assert_eq!(
        report.source_changed, 1,
        "a torn descendant must reach the tree report — `move_tree` \
         reads exactly this field to decide whether unlinking the \
         source tree is safe",
    );
}

/// The same guard end-to-end through `move_tree`: a torn descendant
/// degrades the move to a copy and the whole source tree survives.
///
/// `move_tree` renames atomically whenever it can, and only falls back
/// to copy-then-delete on a genuine `CrossesDevices` error, so this
/// needs two real filesystems. Same constraint (and the same loud
/// skip) as the single-file move test above; the portable half of the
/// coverage is the tally test.
#[tokio::test]
async fn a_cross_device_move_tree_with_a_torn_descendant_keeps_the_source() {
    let Some((src_root, dst_root)) = cross_device_pair() else {
        eprintln!(
            "SKIP a_cross_device_move_tree_with_a_torn_descendant_keeps_the_source: \
             no cross-device pair on this host"
        );
        return;
    };
    let src_dir = src_root.join("tree");
    let dst_dir = dst_root.join("tree");
    let _ = std::fs::remove_dir_all(&src_dir);
    let _ = std::fs::remove_dir_all(&dst_dir);
    tokio::fs::create_dir_all(src_dir.join("nested"))
        .await
        .unwrap();

    let calm = src_dir.join("calm.bin");
    let busy = src_dir.join("nested").join("busy.log");
    write(&calm, &vec![3u8; 8192]).await;
    write(&busy, b"first generation").await;

    let opts = MoveOptions {
        copy: CopyOptions {
            source_stability: SourceStability::Warn,
            shape: Some(Arc::new(TearOnFirstRead::on_read_len(
                &busy,
                b"SECOND GENERATION",
                b"first generation".len() as u64,
            ))),
            ..CopyOptions::default()
        },
        ..MoveOptions::default()
    };

    let (tx, mut rx) = mpsc::channel(512);
    let handle = tokio::spawn({
        let src_dir = src_dir.clone();
        let dst_dir = dst_dir.clone();
        async move { freally_core::move_tree(&src_dir, &dst_dir, opts, CopyControl::new(), tx).await }
    });
    while rx.recv().await.is_some() {}
    let report = handle
        .await
        .expect("move task joined")
        .expect("a torn descendant degrades the move to a copy, it does not fail");

    assert_eq!(report.source_changed, 1);
    assert!(
        busy.exists(),
        "THE data-loss case: the torn file's source is the only coherent \
         copy left and must survive the move",
    );
    assert!(
        calm.exists(),
        "the deletion walker is path-driven and cannot tell which \
         descendants tore, so a tear anywhere keeps the whole tree",
    );
    let _ = std::fs::remove_dir_all(&src_dir);
    let _ = std::fs::remove_dir_all(&dst_dir);
}

/// A cross-device `move_tree` that **skipped** a colliding file must
/// not delete the source tree.
///
/// `move_tree` hardcodes `TreeOptions::default()`, whose collision
/// policy is `Skip`, and its post-copy guard used to check only
/// `source_changed`. So moving onto a partly-populated destination
/// skipped the colliding files — transferring zero bytes — and then
/// permanently unlinked their sources. Not the trash: `remove_file`.
/// The destination kept its *older, different* content and the only
/// copy of the source content was destroyed. That is the common shape
/// of "move my photos onto the backup drive", not an exotic one.
#[tokio::test]
async fn a_cross_device_move_tree_that_skipped_a_file_keeps_the_source() {
    let Some((src_root, dst_root)) = cross_device_pair() else {
        eprintln!(
            "SKIP a_cross_device_move_tree_that_skipped_a_file_keeps_the_source: \
             no cross-device pair on this host"
        );
        return;
    };
    let src_dir = src_root.join("skiptree");
    let dst_dir = dst_root.join("skiptree");
    let _ = std::fs::remove_dir_all(&src_dir);
    let _ = std::fs::remove_dir_all(&dst_dir);
    tokio::fs::create_dir_all(&src_dir).await.unwrap();
    tokio::fs::create_dir_all(&dst_dir).await.unwrap();

    let collides = src_dir.join("photo.jpg");
    let fresh = src_dir.join("notes.txt");
    write(&collides, b"the original, and the only copy").await;
    write(&fresh, b"moves fine").await;
    // A pre-existing destination with *different* content — the
    // collision policy will decline to overwrite it.
    write(&dst_dir.join("photo.jpg"), b"an older, different version").await;

    let (tx, mut rx) = mpsc::channel(512);
    let handle = tokio::spawn({
        let src_dir = src_dir.clone();
        let dst_dir = dst_dir.clone();
        async move {
            freally_core::move_tree(
                &src_dir,
                &dst_dir,
                MoveOptions::default(),
                CopyControl::new(),
                tx,
            )
            .await
        }
    });
    while rx.recv().await.is_some() {}
    let report = handle
        .await
        .expect("move task joined")
        .expect("a skipped collision degrades the move to a copy, it does not fail");

    assert!(
        report.skipped > 0,
        "the collision must be tallied as skipped"
    );
    assert!(
        collides.exists(),
        "THE data-loss case: nothing was copied for this file, so \
         deleting its source destroys the only copy of its content",
    );
    assert!(
        fresh.exists(),
        "the deletion walker is path-driven and cannot tell which files \
         were skipped, so any skip keeps the whole tree",
    );
    let _ = std::fs::remove_dir_all(&src_dir);
    let _ = std::fs::remove_dir_all(&dst_dir);
}

/// A minimal [`freally_core::ProvenanceSink`] that only counts what it
/// was asked to certify. Mirrors the shape of the real sink without
/// pulling `bao` / `ed25519-dalek` into this test binary.
#[derive(Debug, Default)]
struct RecordingProvenanceSink {
    files: AtomicU64,
}

impl RecordingProvenanceSink {
    fn recorded(&self) -> u64 {
        self.files.load(Ordering::SeqCst)
    }
}

impl freally_core::ProvenanceSink for RecordingProvenanceSink {
    fn make_encoder(&self) -> Box<dyn freally_core::OutboardEncoder> {
        Box::new(RootOnlyStub(blake3::Hasher::new()))
    }
    fn record_file(&self, _: &Path, _: &Path, _: u64, _: [u8; 32], _: Vec<u8>) {
        self.files.fetch_add(1, Ordering::SeqCst);
    }
}

struct RootOnlyStub(blake3::Hasher);

impl freally_core::OutboardEncoder for RootOnlyStub {
    fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }
    fn finalize(self: Box<Self>) -> ([u8; 32], Vec<u8>) {
        (*self.0.finalize().as_bytes(), Vec::new())
    }
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
