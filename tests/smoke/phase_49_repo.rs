//! Phase 49 smoke — unified sync + backup repository.
//!
//! Mirrors the brief's smoke spec, one case per assertion:
//!
//! 1. **Unified timeline.** Initialise a [`Repository`], record one
//!    copy, one sync, and one version checkpoint (the "backup-style
//!    snapshot"). The unified snapshot list must report all three, of
//!    all three kinds.
//! 2. **Deduplication.** A file copied, then modified + synced, then
//!    checkpointed, must leave the chunk store smaller than 1.5× the
//!    raw file size — the three near-identical states share chunks.
//! 3. **Garbage collection.** `gc()` over a fully-retained timeline
//!    must delete nothing (no live data lost). After a snapshot is
//!    removed, `gc()` must sweep exactly the chunks that snapshot
//!    uniquely held, while every surviving file still restores
//!    byte-for-byte.
//!
//! All cases run on every host (pure std + redb, no platform gating).
//! `FREALLY_PHASE49_FULL=1` scales the file sizes up for CI's thorough
//! pass.

use std::path::Path;

use freally_chunk::{Repository, SnapshotKind};

/// Deterministic PRNG (LCG) so the smoke is reproducible across hosts —
/// same constants as the Phase 27 smoke. Content-defined chunking only
/// cares that the bytes are non-compressible, not that they're random.
fn seeded_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    let mut s = seed;
    for b in &mut out {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *b = (s >> 33) as u8;
    }
    out
}

fn scale_up() -> bool {
    std::env::var("FREALLY_PHASE49_FULL").is_ok_and(|v| v == "1")
}

#[test]
fn case1_unified_timeline_lists_all_three_kinds() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = Repository::open(tmp.path()).unwrap();

    let size = if scale_up() {
        16 * 1024 * 1024
    } else {
        2 * 1024 * 1024
    };
    let docs = seeded_bytes(0xC0FFEE, size);
    let photos = seeded_bytes(0x5EED, size);

    // 1 copy job, 1 sync run, 1 version checkpoint of a watched folder.
    repo.snapshot_bytes(
        SnapshotKind::Copy,
        "copy /docs -> /backup",
        1_000,
        &[("/docs/report.pdf", &docs)],
    )
    .unwrap();
    repo.snapshot_bytes(
        SnapshotKind::Sync,
        "sync /photos <-> /nas",
        2_000,
        &[("/photos/img.raw", &photos)],
    )
    .unwrap();
    repo.snapshot_bytes(
        SnapshotKind::Version,
        "checkpoint /docs",
        3_000,
        &[("/docs/report.pdf", &docs)],
    )
    .unwrap();

    let list = repo.snapshots().unwrap();
    assert_eq!(list.len(), 3, "unified timeline must hold all 3 snapshots");

    let kinds: std::collections::HashSet<SnapshotKind> = list.iter().map(|s| s.kind).collect();
    assert!(kinds.contains(&SnapshotKind::Copy));
    assert!(kinds.contains(&SnapshotKind::Sync));
    assert!(kinds.contains(&SnapshotKind::Version));

    // Oldest-first ordering.
    assert_eq!(list[0].created_at_ms, 1_000);
    assert_eq!(list[2].created_at_ms, 3_000);

    // Stats: effective bytes counts every snapshot's logical size; the
    // store deduplicates the repeated /docs content.
    let stats = repo.stats().unwrap();
    assert_eq!(stats.snapshot_count, 3);
    assert_eq!(stats.effective_bytes, 3 * size as u64);
    assert!(
        stats.stored_bytes < stats.effective_bytes,
        "dedup must store fewer bytes ({}) than the effective {}",
        stats.stored_bytes,
        stats.effective_bytes
    );
    assert!(stats.saved_ratio() > 0.0);
}

#[test]
fn case2_dedup_keeps_store_under_1_5x_raw() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = Repository::open(tmp.path()).unwrap();

    let raw = if scale_up() {
        64 * 1024 * 1024
    } else {
        16 * 1024 * 1024
    };
    let v1 = seeded_bytes(0xBA5E, raw);
    // Modify: flip a single byte in the middle. FastCDC keeps every
    // chunk except the one (or two) around the mutation.
    let mut v2 = v1.clone();
    v2[raw / 2] ^= 0x5a;

    repo.snapshot_bytes(
        SnapshotKind::Copy,
        "copy v1",
        1_000,
        &[("/data/a.bin", &v1)],
    )
    .unwrap();
    repo.snapshot_bytes(
        SnapshotKind::Sync,
        "sync v2",
        2_000,
        &[("/data/a.bin", &v2)],
    )
    .unwrap();
    // Checkpoint of the already-synced v2 → fully dedup, adds nothing.
    repo.snapshot_bytes(
        SnapshotKind::Version,
        "checkpoint v2",
        3_000,
        &[("/data/a.bin", &v2)],
    )
    .unwrap();

    let stored = repo.store().disk_usage_bytes().unwrap();
    let bar = raw as u64 + raw as u64 / 2; // 1.5× the raw single-file size
    assert!(
        stored < bar,
        "store {stored} must be < 1.5× raw ({bar}); three near-identical \
         states should share almost all chunks",
    );
}

#[test]
fn case3_gc_reclaims_orphans_preserves_live() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = Repository::open(tmp.path()).unwrap();

    let size = if scale_up() {
        16 * 1024 * 1024
    } else {
        2 * 1024 * 1024
    };
    let a = seeded_bytes(0xA11CE, size);
    let b = seeded_bytes(0xB0B, size);
    // First chunk hash of `a` — a witness that a-only chunks vanish.
    let a_chunk = freally_chunk::Chunker::default().chunk_bytes(&a)[0].hash;

    let s_a = repo
        .snapshot_bytes(SnapshotKind::Copy, "snap a", 1_000, &[("/a", &a)])
        .unwrap();
    repo.snapshot_bytes(SnapshotKind::Version, "snap b", 2_000, &[("/b", &b)])
        .unwrap();
    assert!(repo.store().has(&a_chunk).unwrap());

    // gc() over the full, fully-retained timeline must delete NOTHING.
    let untouched = repo.gc().unwrap();
    assert_eq!(untouched.chunks_swept, 0, "gc must not touch live data");
    assert!(repo.store().has(&a_chunk).unwrap());

    // Remove snapshot A → its unique chunks become orphans.
    assert!(repo.remove_snapshot(s_a).unwrap());
    let report = repo.gc().unwrap();
    assert!(
        report.chunks_swept >= 1,
        "gc must sweep the orphaned chunks"
    );
    assert!(
        !repo.store().has(&a_chunk).unwrap(),
        "orphan chunk must be gone after gc"
    );
    assert_eq!(repo.chunk_refcount(&a_chunk).unwrap(), 0);

    // No live data lost: B is still listed and still restores exactly.
    let list = repo.snapshots().unwrap();
    assert_eq!(list.len(), 1);
    let fs = repo.snapshot_at("/b", i64::MAX).unwrap().unwrap();
    let dst: &Path = &tmp.path().join("restored-b.bin");
    repo.restore(&fs, dst).unwrap();
    assert_eq!(
        blake3::hash(&std::fs::read(dst).unwrap()).as_bytes(),
        blake3::hash(&b).as_bytes(),
        "restored B must match the original byte-for-byte",
    );
}
