//! A symlinked / junctioned sync root must fail the pass, never scan as
//! an empty listing.
//!
//! `scan_side`'s own pre-flight uses `exists()` / `is_dir()`, both of
//! which follow the link, so a junction root passed. With
//! `follow_root_links(false)` the walk then yields only the root entry,
//! which the loop skips as "not a file" — producing `Ok(empty map)`.
//!
//! That is the most dangerous possible result here: the engine reads it
//! as "every file on this side was deleted" and a mirror pass emits
//! `SyncAction::Delete` for each one, wiping the other side. Hence the
//! explicit refusal this test locks in.

use std::path::Path;

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

#[test]
fn scan_side_refuses_a_symlinked_root() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::write(real.join("a.txt"), b"a").unwrap();

    let link = tmp.path().join("link");
    if !make_dir_link(&real, &link) {
        eprintln!("skipping: this platform/session cannot create directory links");
        return;
    }

    let out = freally_sync::walker::scan_side(&link, &[]);
    assert!(
        out.is_err(),
        "a symlinked sync root must error, not return an empty listing that \
         the engine would mirror as mass deletion",
    );
}

#[test]
fn scan_side_still_reads_a_real_root() {
    // Guard against over-rotating: an ordinary root must still scan.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("side");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.txt"), b"a").unwrap();

    let out = freally_sync::walker::scan_side(&root, &[]).expect("a real root must scan");
    assert!(
        out.contains_key("a.txt"),
        "expected a.txt in the listing, got {:?}",
        out.keys().collect::<Vec<_>>()
    );
}
