//! Phase 50 smoke — CDR-0 cross-tool repository portability.
//!
//! The full Phase 50 vision (a `copythat migrate restic|borg|kopia` /
//! `copythat export kopia` CLI, a sibling `cdr-py` PyPI package, and
//! upstream adoption PRs) needs third-party repository binaries, a
//! separately published crate, and community engagement — none of which
//! a self-contained `cargo test` can exercise. Those are tracked as
//! deferred follow-ups in `docs/ROADMAP.md`.
//!
//! What this smoke *does* gate is the on-disk **format contract** every
//! one of those tools builds on: the CDR-0 canonical manifest. A
//! manifest produced by this repo's [`Repository`] (Phase 49) must
//! round-trip losslessly through canonical CBOR, validate, preserve its
//! content-addressed chunks byte-for-byte, and honour the spec's
//! "refuse to read a newer repo" rule.
//!
//! Cases:
//! 1. **Round-trip fidelity.** Repository manifest → CDR-0 → CBOR →
//!    CDR-0 → Repository manifest is the identity, and the canonical
//!    encoding is deterministic.
//! 2. **Chunk reuse.** Every chunk the CDR-0 manifest references is
//!    already present in the store — migration translates *manifests*,
//!    it never duplicates chunk bytes.
//! 3. **Version refusal + validation.** A manifest tagged with a newer
//!    `spec_version`, or with a corrupt digest / broken tiling, is
//!    rejected rather than mis-parsed.

use copythat_chunk::cdr::{CDR_SPEC_VERSION, CdrManifest};
use copythat_chunk::{Repository, SnapshotKind, ensure_readable};

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
    std::env::var("COPYTHAT_PHASE50_FULL").is_ok_and(|v| v == "1")
}

/// Capture a file into a fresh repository and return its internal
/// manifest, resolved through the Phase 49 query API.
fn repo_manifest(tmp: &std::path::Path) -> copythat_chunk::Manifest {
    let repo = Repository::open(tmp).unwrap();
    let size = if scale_up() {
        32 * 1024 * 1024
    } else {
        4 * 1024 * 1024
    };
    let bytes = seeded_bytes(0x0CD0_5EED, size);
    repo.snapshot_bytes(
        SnapshotKind::Backup,
        "portability",
        1_000,
        &[("/vault/a.bin", &bytes)],
    )
    .unwrap();
    let fs = repo.snapshot_at("/vault/a.bin", i64::MAX).unwrap().unwrap();
    // Every chunk the manifest names must already live in the store —
    // the migration story is "reuse the chunks, translate the manifest".
    for c in &fs.manifest.chunks {
        assert!(
            repo.store().has(&c.hash).unwrap(),
            "manifest chunk missing from store — chunks must be reused, not duplicated",
        );
    }
    fs.manifest
}

#[test]
fn case1_manifest_round_trips_through_cdr0() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = repo_manifest(tmp.path());
    assert!(!manifest.chunks.is_empty());

    let cdr = CdrManifest::from_manifest(&manifest);
    cdr.validate()
        .expect("freshly built CDR manifest must validate");

    let cbor = cdr.to_cbor().unwrap();
    // Deterministic: re-encoding the same manifest is byte-identical
    // (two compliant tools agree on a manifest's identity).
    assert_eq!(
        cbor,
        cdr.to_cbor().unwrap(),
        "canonical CBOR must be deterministic"
    );

    let decoded = CdrManifest::from_cbor(&cbor).unwrap();
    assert_eq!(decoded, cdr, "CBOR decode must reproduce the CDR manifest");

    // Lossless back to the internal form the restore path consumes.
    let back = decoded.to_manifest().unwrap();
    assert_eq!(back, manifest, "CDR-0 round-trip must be the identity");
}

#[test]
fn case2_reader_refuses_newer_spec_version() {
    // The free-function gate.
    assert!(ensure_readable(CDR_SPEC_VERSION).is_ok());
    assert!(ensure_readable(CDR_SPEC_VERSION + 1).is_err());

    // A manifest claiming a future spec version must be refused by the
    // validating reader at decode time (and by the raw decode + to_manifest
    // path too), never silently trusted.
    let tmp = tempfile::tempdir().unwrap();
    let manifest = repo_manifest(tmp.path());
    let mut cdr = CdrManifest::from_manifest(&manifest);
    cdr.spec_version = CDR_SPEC_VERSION + 1;
    let cbor = cdr.to_cbor().unwrap();
    assert!(
        CdrManifest::from_cbor(&cbor).is_err(),
        "the validating reader must refuse a newer spec version at decode",
    );
    let raw = CdrManifest::from_cbor_unchecked(&cbor).unwrap();
    assert!(raw.to_manifest().is_err(), "to_manifest must also refuse");
}

#[test]
fn case3_validation_rejects_corruption() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = repo_manifest(tmp.path());

    // Truncated digest.
    let mut bad_hash = CdrManifest::from_manifest(&manifest);
    bad_hash.file_hash.truncate(16);
    assert!(bad_hash.validate().is_err());

    // Broken contiguous tiling.
    let mut bad_tile = CdrManifest::from_manifest(&manifest);
    if let Some(last) = bad_tile.chunks.last_mut() {
        last.offset += 1;
    }
    assert!(bad_tile.validate().is_err());

    // Size that disagrees with the chunk lengths.
    let mut bad_size = CdrManifest::from_manifest(&manifest);
    bad_size.size += 1;
    assert!(bad_size.validate().is_err());
}
