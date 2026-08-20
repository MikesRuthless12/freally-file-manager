//! A symlinked / junctioned tree root must be refused loudly, never
//! walked into a silent zero-file "success".
//!
//! Background: walkdir defaults `follow_root_links` to **true**, so
//! `follow_links(false)` never covered the root — a junction root was
//! descended into and its target copied out of the tree. Setting
//! `follow_root_links(false)` stops that, but on its own it produces a
//! far worse failure: the walk yields only the root entry, the
//! enumerator discards it, and the job reports `files: 0` as success.
//! In `move_tree` the delete half would then run against a source the
//! copy half never read, and in sync an empty listing becomes
//! `SyncAction::Delete` for every file on the other side.
//!
//! So the contract is: refuse, with an error that says why.

use std::path::Path;

use freally_core::{CopyControl, TreeOptions, copy_tree};
use tokio::sync::mpsc;

/// Create a directory symlink/junction, or return false when the
/// platform will not let us (unprivileged Windows without Developer
/// Mode). The test is skipped in that case rather than failing for an
/// unrelated reason.
fn make_dir_link(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, link).is_ok()
    }
}

#[tokio::test]
async fn copy_tree_refuses_a_symlinked_root_instead_of_copying_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    std::fs::create_dir_all(real.join("sub")).unwrap();
    std::fs::write(real.join("a.txt"), b"a").unwrap();
    std::fs::write(real.join("sub").join("b.txt"), b"b").unwrap();

    let link = tmp.path().join("link");
    if !make_dir_link(&real, &link) {
        eprintln!("skipping: this platform/session cannot create directory links");
        return;
    }

    let dst = tmp.path().join("out");
    let (tx, _rx) = mpsc::channel(64);
    let opts = TreeOptions::default();
    assert!(
        !opts.follow_symlinks_in_tree,
        "default must be do-not-follow for this test to mean anything",
    );

    let result = copy_tree(&link, &dst, opts, CopyControl::new(), tx).await;

    // The bug being locked out is `Ok(report)` with `files == 0`.
    match result {
        Ok(report) => panic!(
            "a symlinked root must not report success; got files={} bytes={}",
            report.files, report.bytes
        ),
        Err(e) => assert!(
            e.message.contains("symlink") || e.message.contains("junction"),
            "error should name the cause, got: {}",
            e.message
        ),
    }
}

#[tokio::test]
async fn copy_tree_still_copies_a_real_directory_root() {
    // Guard against over-rotating: an ordinary root must be unaffected.
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("a.txt"), b"hello").unwrap();
    std::fs::write(src.join("sub").join("b.txt"), b"world").unwrap();

    let dst = tmp.path().join("out");
    let (tx, mut rx) = mpsc::channel(1024);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });

    let report = copy_tree(&src, &dst, TreeOptions::default(), CopyControl::new(), tx)
        .await
        .expect("an ordinary directory root must still copy");
    drain.await.unwrap();

    assert_eq!(report.files, 2, "both files should be copied");
    assert!(dst.join("a.txt").is_file());
    assert!(dst.join("sub").join("b.txt").is_file());
}
