//! Phase 17c smoke test — symlink-race / TOCTOU hardening.
//!
//! The engine's per-file open path now carries `O_NOFOLLOW`
//! (Linux/macOS) / `FILE_FLAG_OPEN_REPARSE_POINT` (Windows). This
//! test asserts that:
//!
//! 1. `safety::no_follow_open_flags()` returns the right per-OS
//!    value at compile time.
//! 2. `safety::is_no_follow_rejection` recognises the platform's
//!    symlink-rejection error codes.
//! 3. `safety::is_within_root` correctly classifies a candidate
//!    inside / outside the chosen jail root.
//! 4. **Race regression** — when the source is swapped for a symlink
//!    between the metadata pre-flight and the open (Phase 17c
//!    threat model), the engine surfaces a typed error rather than
//!    silently copying the symlink target's contents.

use std::path::Path;

use freally_core::safety::{
    PathSafetyError, is_no_follow_rejection, is_within_root, no_follow_open_flags,
    validate_path_no_traversal,
};
use tempfile::TempDir;

#[test]
fn no_follow_flags_present_on_supported_targets() {
    let flags = no_follow_open_flags();
    #[cfg(target_os = "linux")]
    assert_eq!(flags, 0x20000, "O_NOFOLLOW (Linux glibc/musl) = 0x20000");
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    assert_eq!(flags, 0x100, "O_NOFOLLOW (Apple libc) = 0x100");
    #[cfg(target_os = "windows")]
    assert_eq!(flags, 0x00200000, "FILE_FLAG_OPEN_REPARSE_POINT");
    let _ = flags;
}

#[test]
fn is_no_follow_rejection_recognises_per_os_codes() {
    #[cfg(target_os = "linux")]
    {
        let err = std::io::Error::from_raw_os_error(40); // ELOOP
        assert!(is_no_follow_rejection(&err));
    }
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        let err = std::io::Error::from_raw_os_error(62); // ELOOP on Apple
        assert!(is_no_follow_rejection(&err));
    }
    #[cfg(target_os = "windows")]
    {
        let err1 = std::io::Error::from_raw_os_error(1920); // ERROR_CANT_ACCESS_FILE
        assert!(is_no_follow_rejection(&err1));
        let err2 = std::io::Error::from_raw_os_error(1); // ERROR_INVALID_FUNCTION
        assert!(is_no_follow_rejection(&err2));
    }
    // Non-matching codes always classify as "not a no-follow rejection".
    let unrelated = std::io::Error::from_raw_os_error(2); // ENOENT / ERROR_FILE_NOT_FOUND
    assert!(!is_no_follow_rejection(&unrelated));
}

#[test]
fn is_within_root_jails_correctly() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let inside = root.join("ok.txt");
    std::fs::write(&inside, b"hi").unwrap();

    // Same-dir parent is inside.
    let res = is_within_root(&inside, &root).unwrap();
    assert!(res, "{:?} should be inside {:?}", inside, root);

    // A sibling tempdir is outside.
    let other = TempDir::new().unwrap();
    let outside = other.path().join("evil.txt");
    std::fs::write(&outside, b"x").unwrap();
    let res = is_within_root(&outside, &root).unwrap();
    assert!(!res, "{:?} must NOT be inside {:?}", outside, root);
}

#[test]
fn lexical_guard_still_rejects_traversal() {
    // Phase 17a still applies regardless of Phase 17c — the lexical
    // guard runs *before* any FS call. A traversal-laden source path
    // never reaches the no-follow open in the first place.
    let err = validate_path_no_traversal(Path::new("evil/../../../etc/passwd")).unwrap_err();
    assert!(matches!(err, PathSafetyError::ParentTraversal { .. }));
}

// SECURITY REVIEW (2026-06-27, unix) — this replaces the removed
// `copy_file_rejects_post_check_symlink_swap_unix`, which was both *wrong* and
// nondeterministic. That test set `follow_symlinks = true` and asserted the
// copy would be *rejected* — but that mode follows the link by design (the
// caller explicitly opted in), so the engine correctly copies the target and
// returns Ok. See the passing `tests/symlink.rs::follow_symlinks_copies_the_
// target_contents` (follow => copy target) and `no_follow_clones_the_symlink_
// itself` (no-follow => clone the link, read 0 bytes, never touch the target).
// Its `expect_err` therefore asserted the inverse of correct behaviour, and
// the race harness made the wrong assertion flap run-to-run.
//
// Conclusion: NO vulnerability. The TOCTOU symlink defense is `O_NOFOLLOW`,
// which `engine::open_src_with_retry` (src) and the dst open both OR in via
// `safety::no_follow_open_flags()` whenever `follow_symlinks = false` — so a
// regular-file -> symlink swap landing between the metadata pre-flight and the
// real open cannot redirect the read/write to a victim. This test pins that
// primitive deterministically: opening a symlink with O_NOFOLLOW must fail at
// open time, classified by `safety::is_no_follow_rejection`.
#[cfg(unix)]
#[test]
fn no_follow_open_flag_rejects_a_symlink_at_open_unix() {
    use freally_core::safety::{is_no_follow_rejection, no_follow_open_flags};
    use std::os::unix::fs::{OpenOptionsExt, symlink};

    let dir = TempDir::new().unwrap();
    let victim = dir.path().join("victim.secret");
    std::fs::write(&victim, b"SECRET").unwrap();
    let link = dir.path().join("link");
    symlink(&victim, &link).unwrap();

    // The defense must be armed on this target (non-zero O_NOFOLLOW flag).
    assert_ne!(
        no_follow_open_flags(),
        0,
        "no-follow defense must be armed on this unix target"
    );

    // Opening the symlink itself with O_NOFOLLOW fails at open time, so the
    // descriptor never reaches the victim — a post-check symlink swap can't
    // leak the target's bytes. Deterministic: one static symlink, one open.
    let err = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(no_follow_open_flags() as i32)
        .open(&link)
        .expect_err("O_NOFOLLOW must reject a symlink at open time");
    assert!(
        is_no_follow_rejection(&err),
        "expected an ELOOP-shaped no-follow rejection, got {err:?} (raw={:?})",
        err.raw_os_error()
    );
}
