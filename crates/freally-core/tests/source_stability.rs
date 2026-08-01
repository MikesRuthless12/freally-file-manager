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

use std::path::Path;
use std::time::Duration;

use freally_core::stability::{SourceStability, SourceStamp, StabilityVerdict, evaluate};
use freally_core::{CopyControl, CopyEvent, CopyOptions, copy_file};
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

    let before = SourceStamp::of(&src).await.expect("stat before");
    write(&src, b"second generation, longer than the first").await;
    let after = SourceStamp::of(&src).await.expect("stat after");

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
    let before = SourceStamp::of(&src).await.expect("stat before");

    // Filesystem mtime granularity can be as coarse as 1-2 s; wait past
    // it so this asserts the guard's logic rather than the host's clock
    // resolution.
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    write(&src, b"BBBBBBBBBBBBBBBB").await;
    let after = SourceStamp::of(&src).await.expect("stat after");

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
    assert!(SourceStamp::of(&missing).await.is_none());

    let dst = dir.path().join("dst.bin");
    let (result, _) = copy_collecting(&missing, &dst, SourceStability::Fail).await;
    let err = result.expect_err("copying a missing source must fail");
    assert!(
        !err.contains("source changed"),
        "a missing source must not be reported as a mid-read change: {err}",
    );
}
