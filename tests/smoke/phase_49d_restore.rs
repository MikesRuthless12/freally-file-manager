//! Phase 49d smoke — restore browser backend.
//!
//! `Repository::snapshot_tree` lists a snapshot's files (path + size, no
//! manifests) for the UI tree; `Repository::restore_paths` writes selected
//! files back under a destination root with overwrite / skip / keep-both
//! conflict handling and a hard traversal guard that can never escape the
//! restore root.

use std::path::Path;

use copythat_chunk::{Repository, RestoreConflict, SnapshotKind};

#[test]
fn case1_snapshot_tree_lists_paths_and_sizes() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = Repository::open(&tmp.path().join("repo")).unwrap();
    let a = vec![0xA1u8; 1000];
    let b = vec![0xB2u8; 2000];
    let c = vec![0xC3u8; 3000];
    let id = repo
        .snapshot_bytes(
            SnapshotKind::Backup,
            "b",
            1_000,
            &[("docs/a.txt", &a), ("docs/b.txt", &b), ("c.bin", &c)],
        )
        .unwrap();

    let mut tree = repo.snapshot_tree(id).unwrap();
    tree.sort_by(|x, y| x.path.cmp(&y.path));
    assert_eq!(tree.len(), 3);
    assert_eq!(tree[0].path, "c.bin");
    assert_eq!(tree[0].size, 3000);
    assert_eq!(tree[1].path, "docs/a.txt");
    assert_eq!(tree[1].size, 1000);
    assert_eq!(tree[2].path, "docs/b.txt");
    assert_eq!(tree[2].size, 2000);
}

#[test]
fn case2_restore_subset_byte_for_byte() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = Repository::open(&tmp.path().join("repo")).unwrap();
    let a = vec![0xA1u8; 4096];
    let b = vec![0xB2u8; 4096];
    let id = repo
        .snapshot_bytes(
            SnapshotKind::Backup,
            "b",
            1_000,
            &[("docs/a.txt", &a), ("b.txt", &b)],
        )
        .unwrap();

    let dst = tmp.path().join("out");
    let rep = repo
        .restore_paths(id, &["docs/a.txt"], &dst, None, RestoreConflict::Overwrite)
        .unwrap();
    assert_eq!(rep.restored, 1);
    assert_eq!(rep.failed, 0);
    assert_eq!(std::fs::read(dst.join("docs/a.txt")).unwrap(), a);
    // The unselected file was not restored.
    assert!(!dst.join("b.txt").exists());
}

#[test]
fn case3_conflict_policies() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = Repository::open(&tmp.path().join("repo")).unwrap();
    let snap_bytes = vec![0x5Au8; 4096];
    let id = repo
        .snapshot_bytes(SnapshotKind::Backup, "b", 1_000, &[("c.bin", &snap_bytes)])
        .unwrap();
    let dst = tmp.path().join("out");

    // Pre-place a different file at the target.
    std::fs::create_dir_all(&dst).unwrap();
    let preexisting = vec![0x11u8; 100];
    std::fs::write(dst.join("c.bin"), &preexisting).unwrap();

    // Skip: existing file preserved.
    let rep = repo
        .restore_paths(id, &["c.bin"], &dst, None, RestoreConflict::Skip)
        .unwrap();
    assert_eq!(rep.skipped, 1);
    assert_eq!(std::fs::read(dst.join("c.bin")).unwrap(), preexisting);

    // KeepBoth: original preserved + a " (1)" sibling carries the snapshot.
    let rep = repo
        .restore_paths(id, &["c.bin"], &dst, None, RestoreConflict::KeepBoth)
        .unwrap();
    assert_eq!(rep.restored, 1);
    assert_eq!(std::fs::read(dst.join("c.bin")).unwrap(), preexisting);
    assert_eq!(std::fs::read(dst.join("c (1).bin")).unwrap(), snap_bytes);

    // Overwrite: snapshot content replaces the existing file.
    let rep = repo
        .restore_paths(id, &["c.bin"], &dst, None, RestoreConflict::Overwrite)
        .unwrap();
    assert_eq!(rep.restored, 1);
    assert_eq!(std::fs::read(dst.join("c.bin")).unwrap(), snap_bytes);
}

#[test]
fn case4_traversal_guard_never_escapes_dst_root() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = Repository::open(&tmp.path().join("repo")).unwrap();
    let evil = vec![0xEEu8; 256];
    // A malicious snapshot path that tries to climb out of the dst root.
    let id = repo
        .snapshot_bytes(
            SnapshotKind::Backup,
            "evil",
            1_000,
            &[("../escape.txt", &evil)],
        )
        .unwrap();

    let dst = tmp.path().join("restore_root");
    std::fs::create_dir_all(&dst).unwrap();
    let rep = repo
        .restore_paths(
            id,
            &["../escape.txt"],
            &dst,
            None,
            RestoreConflict::Overwrite,
        )
        .unwrap();

    // Refused — nothing written, and definitely nothing outside dst_root.
    assert_eq!(rep.restored, 0);
    assert_eq!(rep.failed, 1);
    let escaped: &Path = &tmp.path().join("escape.txt");
    assert!(!escaped.exists(), "traversal escaped the restore root!");
}
