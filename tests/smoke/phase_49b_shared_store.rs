//! Phase 49b smoke — the shared-store invariant.
//!
//! 49b makes a *single* open `ChunkStore` back the unified [`Repository`],
//! the delta-resume [`CopyThatChunkSink`], and (in the app) the recovery
//! web UI + mount — one `index.redb` lock taken once. These cases prove
//! that contract at the `copythat-chunk` layer; the runner + Tauri command
//! wiring is exercised by the app's own build + manual QA.
//!
//! 1. **One open, many consumers.** `Repository::with_store(Arc<ChunkStore>)`
//!    and `CopyThatChunkSink::new(Arc<ChunkStore>)` built from the SAME
//!    handle coexist with no second `open` (which would deadlock redb's
//!    exclusive lock); a chunk written through one is visible through the
//!    other; `store_arc()` hands back the very same allocation.
//! 2. **Version snapshot via the shared store** — what the app's
//!    `RepositoryVersioningSink` records on overwrite — lands as exactly
//!    one `Version` row and restores byte-for-byte.
//! 3. **GC roots include the Phase 27 manifests** on a `with_store`-built
//!    repo: a chunk only a delta-resume manifest references survives `gc`.

use std::sync::Arc;

use copythat_chunk::{ChunkStore, Chunker, CopyThatChunkSink, Repository, SnapshotKind};

/// Deterministic, non-compressible bytes (same LCG as the Phase 27/49
/// smokes) so content-defined chunking is reproducible across hosts.
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

#[test]
fn case1_with_store_shares_one_open() {
    let tmp = tempfile::tempdir().unwrap();

    // One open of index.redb, wrapped in an Arc.
    let store = Arc::new(ChunkStore::open(tmp.path()).unwrap());

    // The Repository and the delta-resume sink both build on the SAME
    // handle — no second `ChunkStore::open` anywhere, so redb's exclusive
    // index.redb lock is never contended (a second open would error).
    let repo = Repository::with_store(Arc::clone(&store)).unwrap();
    let _sink = CopyThatChunkSink::new(Arc::clone(&store));

    // A chunk written straight to the store is visible through the
    // repository's view of that same store.
    let bytes = seeded_bytes(0xC0FFEE, 64 * 1024);
    let hash = *blake3::hash(&bytes).as_bytes();
    store.put(hash, &bytes).unwrap();
    assert!(
        repo.store().has(&hash).unwrap(),
        "a chunk put through the shared store must be visible to the repo",
    );

    // store_arc() returns the very same allocation, not a fresh open.
    assert!(
        Arc::ptr_eq(&store, &repo.store_arc()),
        "store_arc() must hand back the shared handle, not a new open",
    );
}

#[test]
fn case2_version_snapshot_via_shared_store() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(ChunkStore::open(tmp.path()).unwrap());
    let repo = Repository::with_store(Arc::clone(&store)).unwrap();

    // Mirror what the app's RepositoryVersioningSink does on overwrite:
    // capture the existing destination file as a Version snapshot.
    let original = seeded_bytes(0xBEEF, 1024 * 1024);
    let dst = tmp.path().join("doc.bin");
    std::fs::write(&dst, &original).unwrap();

    let label = dst.to_string_lossy().into_owned();
    repo.snapshot_files(
        SnapshotKind::Version,
        &label,
        1_000,
        &[(label.as_str(), &dst)],
    )
    .unwrap();

    let snaps = repo.snapshots().unwrap();
    assert_eq!(snaps.len(), 1, "exactly one snapshot recorded");
    assert_eq!(snaps[0].kind, SnapshotKind::Version);

    // It restores byte-for-byte from the shared store.
    let resolved = repo.snapshot_at(&label, i64::MAX).unwrap().unwrap();
    let out = tmp.path().join("restored.bin");
    repo.restore(&resolved, &out).unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), original);
}

#[test]
fn case3_phase27_manifest_chunks_survive_gc_on_with_store_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(ChunkStore::open(tmp.path()).unwrap());
    let repo = Repository::with_store(Arc::clone(&store)).unwrap();

    // Ingest via the Phase 27 path (writes the `manifests` table) through
    // the shared store — NO Repository snapshot references these chunks.
    let data = seeded_bytes(0x42, 2 * 1024 * 1024);
    let (_stats, manifest) =
        copythat_chunk::ingest_bytes(&store, &Chunker::default(), &data, "/delta/key").unwrap();
    let witness = manifest.chunks[0].hash;
    assert!(store.has(&witness).unwrap());

    // gc must treat the Phase 27 manifest as a root and keep the chunk.
    let report = repo.gc().unwrap();
    assert_eq!(
        report.chunks_swept, 0,
        "Phase 27 manifest chunks are gc roots even with no snapshot",
    );
    assert!(repo.store().has(&witness).unwrap());
}
