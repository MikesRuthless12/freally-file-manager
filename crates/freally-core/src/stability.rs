//! FFM-M23 — source-stability guard.
//!
//! A file that is rewritten while the engine is reading it produces a
//! destination that is **internally inconsistent**: the head came from
//! the old contents, the tail from the new. Nothing else in the engine
//! can see this:
//!
//! - Post-copy verify re-hashes the *current* source against the
//!   destination. If the writer finished before the verify pass, the
//!   source now matches whatever bytes happened to land, and the torn
//!   copy verifies **green**.
//! - Provenance manifests hash the same current source, so they
//!   certify the tear.
//! - Sparseness re-scan compares extents, not contents.
//!
//! Re-stat'ing size and mtime across the read pass is what closes the
//! window — it is exactly rsync's "file has changed as we read it"
//! guard, and it costs one `stat` per file.
//!
//! ## Why this cannot be a hash comparison
//!
//! Hashing the source twice would be authoritative but doubles the
//! read cost of every copy, and still races: a writer can modify the
//! file between the second hash and the return. Size+mtime is the
//! cheap, standard signal; it is a *detector*, not a proof, and the
//! docs say so rather than implying a guarantee.
//!
//! ## Known blind spot
//!
//! A same-size, same-mtime rewrite is invisible here. Filesystems with
//! coarse (1- or 2-second) mtime granularity make that reachable for a
//! fast in-place rewrite. [`SourceStamp`] therefore also carries the
//! inode/file-index where the platform exposes one, so a
//! replace-by-rename — the common "atomic save" pattern in editors —
//! is caught even when size and mtime happen to match.

use std::fs::Metadata;
use std::path::Path;
use std::time::SystemTime;

/// What to do when a source file changed underneath the copy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SourceStability {
    /// Do not re-stat at all. Matches pre-FFM-M23 behaviour.
    Off,
    /// Record and report the change; keep the copied bytes.
    #[default]
    Warn,
    /// Copy the file once more, then fall back to `Warn` if the source
    /// is still moving.
    Recopy,
    /// Fail the item so nothing downstream treats the bytes as good.
    Fail,
}

/// A cheap fingerprint of a source file, taken before and after the
/// read pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceStamp {
    /// File length in bytes.
    pub len: u64,
    /// Last-modified time, when the platform reports one.
    pub modified: Option<SystemTime>,
    /// A platform identity signal, when one is available on stable
    /// Rust: the inode on Unix, the creation time on Windows. Catches
    /// a replace-by-rename that preserved both size and mtime.
    ///
    /// **Windows caveat:** NTFS file-system tunneling re-applies the
    /// original creation time to a file recreated with the same name
    /// within ~15 seconds, which is exactly the atomic-save pattern.
    /// So on Windows this closes the slow case but not the fast one;
    /// the size and mtime checks remain the primary signal. (The
    /// authoritative field, `MetadataExt::file_index`, is still
    /// nightly-only behind `windows_by_handle`.)
    pub file_id: Option<u64>,
}

impl SourceStamp {
    /// Build a stamp from already-read metadata.
    pub fn from_metadata(md: &Metadata) -> Self {
        Self {
            len: md.len(),
            modified: md.modified().ok(),
            file_id: platform_file_id(md),
        }
    }

    /// Stat `path` and stamp it, resolving symlinks only when the copy
    /// itself will.
    ///
    /// `copy_file_once` reads the source through `symlink_metadata`
    /// when `follow_symlinks` is false, so always following here would
    /// fingerprint the *target* of a link the engine never reads: a
    /// target rewritten mid-copy would report a false change on a
    /// perfectly copied link, and a dangling link would stamp `None`
    /// and silently disable the guard altogether.
    ///
    /// `None` when the file cannot be stat'ed — a source that vanished
    /// mid-copy is reported by the engine's own I/O error path, not
    /// here.
    pub async fn of(path: &Path, follow_symlinks: bool) -> Option<Self> {
        let md = if follow_symlinks {
            tokio::fs::metadata(path).await
        } else {
            tokio::fs::symlink_metadata(path).await
        };
        md.ok().map(|md| Self::from_metadata(&md))
    }

    /// Human-readable summary of what differs, for the event payload
    /// and the history marker. Empty string when the stamps agree.
    pub fn describe_change(&self, after: &SourceStamp) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.len != after.len {
            parts.push(format!("size {} → {}", self.len, after.len));
        }
        if self.modified != after.modified {
            parts.push("mtime changed".to_string());
        }
        if self.file_id != after.file_id {
            parts.push("file replaced".to_string());
        }
        parts.join(", ")
    }
}

#[cfg(unix)]
fn platform_file_id(md: &Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(md.ino())
}

#[cfg(windows)]
fn platform_file_id(md: &Metadata) -> Option<u64> {
    use std::os::windows::fs::MetadataExt;
    // Creation time stands in for the file index, which is still
    // nightly-only. `0` means "not reported" — treat it as no signal
    // rather than as an identity that every unreported file shares.
    let created = md.creation_time();
    (created != 0).then_some(created)
}

#[cfg(not(any(unix, windows)))]
fn platform_file_id(_md: &Metadata) -> Option<u64> {
    None
}

/// What the caller should do, given the two stamps and the policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StabilityVerdict {
    /// The source held still (or the policy is `Off`, or a stamp was
    /// unavailable). Proceed as normal.
    Stable,
    /// The source moved. Report it and keep the bytes.
    Warn,
    /// The source moved. Copy it again, once.
    Recopy,
    /// The source moved. Fail the item.
    Fail,
}

/// Compare `before` and `after` under `policy`.
///
/// A missing stamp on either side yields [`StabilityVerdict::Stable`]:
/// the guard is an *additional* detector, and a platform that cannot
/// produce metadata must not start failing copies that the engine
/// otherwise completed successfully.
pub fn evaluate(
    before: Option<SourceStamp>,
    after: Option<SourceStamp>,
    policy: SourceStability,
) -> StabilityVerdict {
    if policy == SourceStability::Off {
        return StabilityVerdict::Stable;
    }
    let (Some(before), Some(after)) = (before, after) else {
        return StabilityVerdict::Stable;
    };
    if before == after {
        return StabilityVerdict::Stable;
    }
    match policy {
        SourceStability::Off => StabilityVerdict::Stable,
        SourceStability::Warn => StabilityVerdict::Warn,
        SourceStability::Recopy => StabilityVerdict::Recopy,
        SourceStability::Fail => StabilityVerdict::Fail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn stamp(len: u64, secs: u64, id: u64) -> SourceStamp {
        SourceStamp {
            len,
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs)),
            file_id: Some(id),
        }
    }

    #[test]
    fn an_unchanged_source_is_stable_under_every_policy() {
        let s = stamp(100, 5, 7);
        for policy in [
            SourceStability::Off,
            SourceStability::Warn,
            SourceStability::Recopy,
            SourceStability::Fail,
        ] {
            assert_eq!(
                evaluate(Some(s), Some(s), policy),
                StabilityVerdict::Stable,
                "{policy:?}"
            );
        }
    }

    #[test]
    fn off_never_reports_a_change_even_when_the_file_moved() {
        assert_eq!(
            evaluate(
                Some(stamp(100, 5, 7)),
                Some(stamp(200, 9, 7)),
                SourceStability::Off
            ),
            StabilityVerdict::Stable,
        );
    }

    #[test]
    fn each_policy_maps_to_its_own_verdict() {
        let before = Some(stamp(100, 5, 7));
        let after = Some(stamp(120, 5, 7));
        assert_eq!(
            evaluate(before, after, SourceStability::Warn),
            StabilityVerdict::Warn
        );
        assert_eq!(
            evaluate(before, after, SourceStability::Recopy),
            StabilityVerdict::Recopy
        );
        assert_eq!(
            evaluate(before, after, SourceStability::Fail),
            StabilityVerdict::Fail
        );
    }

    #[test]
    fn an_mtime_bump_alone_counts_as_a_change() {
        // The in-place rewrite that keeps the length identical is the
        // exact case a size-only check would miss.
        assert_eq!(
            evaluate(
                Some(stamp(100, 5, 7)),
                Some(stamp(100, 6, 7)),
                SourceStability::Warn
            ),
            StabilityVerdict::Warn,
        );
    }

    #[test]
    fn a_replace_by_rename_is_caught_even_with_identical_size_and_mtime() {
        // Editors' "atomic save": write a temp file, rename it over the
        // original. Size and mtime can match on a coarse-granularity
        // filesystem; the inode cannot.
        assert_eq!(
            evaluate(
                Some(stamp(100, 5, 7)),
                Some(stamp(100, 5, 8)),
                SourceStability::Warn
            ),
            StabilityVerdict::Warn,
        );
    }

    #[test]
    fn a_missing_stamp_never_turns_a_good_copy_into_a_failure() {
        let s = Some(stamp(100, 5, 7));
        assert_eq!(
            evaluate(None, s, SourceStability::Fail),
            StabilityVerdict::Stable
        );
        assert_eq!(
            evaluate(s, None, SourceStability::Fail),
            StabilityVerdict::Stable
        );
        assert_eq!(
            evaluate(None, None, SourceStability::Fail),
            StabilityVerdict::Stable
        );
    }

    #[test]
    fn absent_platform_ids_do_not_manufacture_a_change() {
        // Windows can report `None` for the file index. Two `None`s
        // must compare equal, not "changed".
        let before = SourceStamp {
            len: 10,
            modified: Some(SystemTime::UNIX_EPOCH),
            file_id: None,
        };
        assert_eq!(
            evaluate(Some(before), Some(before), SourceStability::Fail),
            StabilityVerdict::Stable,
        );
    }

    #[test]
    fn the_change_description_names_every_differing_field() {
        let before = stamp(100, 5, 7);
        assert_eq!(
            before.describe_change(&stamp(200, 9, 8)),
            "size 100 → 200, mtime changed, file replaced"
        );
        assert_eq!(before.describe_change(&stamp(200, 5, 7)), "size 100 → 200");
        assert_eq!(before.describe_change(&before), "");
    }

    #[test]
    fn the_default_policy_is_warn() {
        // Warn, not Fail: a torn copy must be reported loudly, but the
        // engine must not delete the only bytes the user managed to
        // rescue off a file something else is actively writing.
        assert_eq!(SourceStability::default(), SourceStability::Warn);
    }
}
