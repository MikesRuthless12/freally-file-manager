//! Phase 49c smoke — backup-source snapshots into the Repository.
//!
//! A backup is a `SnapshotKind::Backup` snapshot recorded via
//! `Repository::snapshot_files` — exactly what `backup_commands::backup_now`
//! calls after enumerating a source tree. These cases prove the two
//! properties the feature rests on at the chunk layer:
//!
//! 1. A backup records one `Backup` snapshot covering every file, and each
//!    file restores byte-for-byte.
//! 2. Re-running the backup on an unchanged tree adds ~no stored bytes
//!    while the effective (logical) size doubles — the dedup hero ratio
//!    rises, so repeat backups are cheap.
//! 3. Changing one file adds only that file's altered chunk(s), not the
//!    whole tree again — true incremental backup.

use std::path::{Path, PathBuf};

use freally_chunk::{Repository, SnapshotKind};

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

/// Build a 3-file source tree under `dir`; return `(relkey, abs_path)`
/// pairs (the shape `backup_now` passes to `snapshot_files`). 4 MiB files
/// so FastCDC yields several chunks each — a single-byte edit then only
/// re-chunks its own region, not the whole file.
fn seed_tree(dir: &Path) -> Vec<(String, PathBuf)> {
    let files = [
        ("docs/a.bin", seeded_bytes(1, 4 * 1024 * 1024)),
        ("docs/b.bin", seeded_bytes(2, 4 * 1024 * 1024)),
        ("photos/c.bin", seeded_bytes(3, 4 * 1024 * 1024)),
    ];
    let mut out = Vec::new();
    for (rel, bytes) in files {
        let abs = dir.join(rel);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, &bytes).unwrap();
        out.push((rel.to_string(), abs));
    }
    out
}

fn pairs(files: &[(String, PathBuf)]) -> Vec<(&str, &Path)> {
    files
        .iter()
        .map(|(k, p)| (k.as_str(), p.as_path()))
        .collect()
}

#[test]
fn case1_backup_records_and_restores() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = Repository::open(&tmp.path().join("repo")).unwrap();
    let src = tmp.path().join("src");
    let files = seed_tree(&src);

    let id = repo
        .snapshot_files(SnapshotKind::Backup, "Documents", 1_000, &pairs(&files))
        .unwrap();

    let snaps = repo.snapshots().unwrap();
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].kind, SnapshotKind::Backup);
    assert_eq!(snaps[0].file_count, 3);

    // Every file restores byte-for-byte through the timeline.
    let full = repo.snapshot(id).unwrap().unwrap();
    for entry in &full.files {
        let original = std::fs::read(src.join(&entry.path)).unwrap();
        let fs = repo.snapshot_at(&entry.path, i64::MAX).unwrap().unwrap();
        let out = tmp.path().join("restore").join(&entry.path);
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        repo.restore(&fs, &out).unwrap();
        assert_eq!(
            std::fs::read(&out).unwrap(),
            original,
            "{} restored mismatch",
            entry.path
        );
    }
}

#[test]
fn case2_incremental_backup_of_unchanged_tree_dedups() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = Repository::open(&tmp.path().join("repo")).unwrap();
    let src = tmp.path().join("src");
    let files = seed_tree(&src);
    let logical: u64 = files
        .iter()
        .map(|(_, p)| std::fs::metadata(p).unwrap().len())
        .sum();

    repo.snapshot_files(SnapshotKind::Backup, "b1", 1_000, &pairs(&files))
        .unwrap();
    let stored1 = repo.stats().unwrap().stored_bytes;

    // The same unchanged tree again — every chunk already in the store.
    repo.snapshot_files(SnapshotKind::Backup, "b2", 2_000, &pairs(&files))
        .unwrap();
    let stats = repo.stats().unwrap();

    assert_eq!(stats.snapshot_count, 2);
    assert_eq!(
        stats.effective_bytes,
        2 * logical,
        "effective must count both backups"
    );
    assert!(
        stats.stored_bytes < stored1 + logical / 4,
        "a second identical backup must add ~no stored bytes (stored {} vs {} + slop)",
        stats.stored_bytes,
        stored1,
    );
    assert!(
        stats.saved_ratio() > 0.4,
        "dedup ratio {} too low for a duplicated backup",
        stats.saved_ratio()
    );
}

#[test]
fn case3_changed_file_adds_only_its_chunks() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = Repository::open(&tmp.path().join("repo")).unwrap();
    let src = tmp.path().join("src");
    let files = seed_tree(&src);
    let one_file = std::fs::metadata(&files[0].1).unwrap().len();

    repo.snapshot_files(SnapshotKind::Backup, "b1", 1_000, &pairs(&files))
        .unwrap();
    let stored1 = repo.stats().unwrap().stored_bytes;

    // Flip one byte in the middle of the first file, then re-back-up.
    let mut bytes = std::fs::read(&files[0].1).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    std::fs::write(&files[0].1, &bytes).unwrap();
    repo.snapshot_files(SnapshotKind::Backup, "b2", 2_000, &pairs(&files))
        .unwrap();
    let stored2 = repo.stats().unwrap().stored_bytes;

    // FastCDC re-chunks only the region around the edit, so the second
    // backup adds far less than a whole fresh file.
    assert!(
        stored2 - stored1 < one_file,
        "incremental backup added {} bytes; should be < one file ({})",
        stored2 - stored1,
        one_file,
    );
}
