//! Post-resolution containment check for the served-file jails.
//!
//! `s3::resolve_key` and `SftpHandler::resolve` are correct *lexical*
//! jails — they reject `..`, percent-encoded `..%2f`, rooted paths, and
//! Windows drive/UNC prefixes. But the path they return is then handed
//! to ordinary filesystem calls (`fs::read`, `File::create`,
//! `OpenOptions::open`) which resolve symlinks and Windows directory
//! junctions. A lexical jail cannot see those: it only inspects the
//! text of the request.
//!
//! So an operator serving `C:\Users\me\Shared` with a junction
//! `docs -> C:\Users\me` (or, on Linux, `docs -> /home/me`) served that
//! entire target tree to any client that asked for `docs/…`, while the
//! S3 listing hid the link — `list_objects` skips symlinks — so the
//! served view gave no hint the path existed.
//!
//! This module closes that by re-checking containment *after* the OS
//! has resolved links.

use std::io;
use std::path::{Path, PathBuf};

/// Fail closed unless `path` really lives under `root` once symlinks and
/// junctions are resolved.
///
/// For a path that does not exist yet (a create/write target) the parent
/// directory is what must be contained — the final component is the name
/// we are about to create.
pub(crate) fn ensure_within_root(path: &Path, root: &Path) -> io::Result<()> {
    let real_root = root.canonicalize()?;

    // Walk up to the nearest ancestor that actually exists and check
    // that. Only probing one level up was wrong: a nested create such as
    // `PUT /bucket/a/b/c.bin` has several missing levels, so
    // `canonicalize` failed with NotFound and the whole request was
    // refused. What must be contained is the deepest real directory the
    // new path will hang off — everything below it is ours to create.
    // Climbing above the root is not guarded here on purpose — comparing
    // a not-yet-canonical parent against the canonical root is unsound
    // (on Windows the latter carries a `\\?\` prefix, so the two never
    // match). The `starts_with(&real_root)` check below is the real gate
    // and catches an ancestor outside the root just the same.
    let mut probe: PathBuf = path.to_path_buf();
    while probe.symlink_metadata().is_err() {
        match probe.parent() {
            Some(p) if p != probe => probe = p.to_path_buf(),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "no existing ancestor for this path",
                ));
            }
        }
    }

    // A `NotFound` here means the path simply is not there — that is not
    // a containment failure, and reporting it as one makes an SFTP
    // client see PERMISSION_DENIED where it expected NO_SUCH_FILE
    // (rclone stats a destination before copying; WinSCP creates nested
    // remote directories). Propagate the kind so the caller can map it.
    let real = probe.canonicalize()?;
    if real.starts_with(&real_root) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} resolves outside the served root {}",
                real.display(),
                real_root.display()
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_path_under_root_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let f = root.join("a.txt");
        std::fs::write(&f, b"x").unwrap();
        assert!(ensure_within_root(&f, root).is_ok());
    }

    #[test]
    fn not_yet_existing_file_is_judged_by_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(ensure_within_root(&root.join("new.txt"), root).is_ok());
    }

    // A nested create has SEVERAL missing levels. Probing only one level
    // up refused these outright, which 403'd every `PUT /bucket/a/b.bin`
    // into a fresh bucket.
    #[test]
    fn deeply_nested_new_path_walks_up_to_the_nearest_real_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let deep = root.join("a").join("b").join("c").join("d.bin");
        assert!(
            ensure_within_root(&deep, root).is_ok(),
            "a nested not-yet-created path under the root must be allowed",
        );
    }

    #[test]
    fn outside_root_is_refused() {
        let inside = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("secret.txt");
        std::fs::write(&victim, b"s").unwrap();
        assert!(ensure_within_root(&victim, inside.path()).is_err());
    }

    // The case the lexical jail cannot see: a link *inside* the root
    // whose target is outside it.
    #[cfg(unix)]
    #[test]
    fn symlink_under_root_pointing_out_is_refused() {
        let inside = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("secret.txt");
        std::fs::write(&victim, b"s").unwrap();
        let link = inside.path().join("innocent.txt");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        assert!(ensure_within_root(&link, inside.path()).is_err());
    }
}
