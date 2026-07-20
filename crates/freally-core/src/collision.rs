//! Destination-already-exists policy.
//!
//! `copy_tree` / `move_tree` consult a `CollisionPolicy` before each
//! file. All decisions except `Prompt` are local and synchronous.
//! `Prompt` emits a `CopyEvent::Collision` and parks the per-file
//! task on a oneshot reply.

use std::path::{Path, PathBuf};

use tokio::sync::{mpsc, oneshot};

use crate::event::{Collision, CollisionResolution, CopyEvent};

/// What to do when `dst` already exists.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CollisionPolicy {
    /// Leave `dst` alone; do not copy this file. The pre-Phase-8
    /// default and the one `Default` resolves to.
    #[default]
    Skip,
    /// Truncate `dst` and overwrite.
    Overwrite,
    /// Overwrite only if `src`'s mtime is strictly newer than `dst`'s.
    OverwriteIfNewer,
    /// Insert a unique suffix (e.g. `foo.txt` → `foo (1).txt`) so both
    /// files exist at the destination.
    KeepBoth,
    /// Use this filename instead of the original. No directory part —
    /// the file still lands in the same destination directory.
    Rename(String),
    /// Ask the caller via a `CopyEvent::Collision` event and wait on
    /// the returned oneshot.
    Prompt,
    /// FFM-M06 — skip when `src` and `dst` are byte-for-byte identical
    /// (size short-circuit, then a streamed content compare); otherwise
    /// fall through to a `Prompt`. Turns a re-run over an existing tree
    /// into "only ask about the files that actually changed".
    SkipIdenticalElsePrompt,
    /// FFM-M06 — skip when `src` and `dst` are byte-for-byte identical;
    /// otherwise overwrite. A fully automatic "sync only what differs"
    /// with no prompts.
    SkipIdenticalElseOverwrite,
}

/// The engine's internal decision after consulting the policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Skip this file. Count it in `skipped`; keep going.
    Skip,
    /// Copy to this destination path, truncating if it exists.
    Write(PathBuf),
    /// Abort the whole tree walk.
    Abort,
}

pub(crate) async fn resolve(
    policy: &CollisionPolicy,
    src: &Path,
    dst: &Path,
    events: &mpsc::Sender<CopyEvent>,
) -> Decision {
    if !path_exists(dst).await {
        return Decision::Write(dst.to_path_buf());
    }
    match policy {
        CollisionPolicy::Skip => Decision::Skip,
        CollisionPolicy::Overwrite => Decision::Write(dst.to_path_buf()),
        CollisionPolicy::OverwriteIfNewer => match source_is_newer(src, dst).await {
            Some(true) => Decision::Write(dst.to_path_buf()),
            _ => Decision::Skip,
        },
        CollisionPolicy::KeepBoth => match keep_both_path(dst).await {
            Some(fresh) => Decision::Write(fresh),
            None => Decision::Skip,
        },
        CollisionPolicy::Rename(name) => {
            let fresh = dst
                .parent()
                .map(|p| p.join(name))
                .unwrap_or_else(|| PathBuf::from(name));
            Decision::Write(fresh)
        }
        CollisionPolicy::Prompt => prompt(src, dst, events).await,
        // FFM-M06 — content-aware auto-policies. Identical content is
        // skipped either way; the two differ only in what happens when
        // the bytes diverge.
        CollisionPolicy::SkipIdenticalElsePrompt => {
            if files_identical(src, dst).await {
                Decision::Skip
            } else {
                prompt(src, dst, events).await
            }
        }
        CollisionPolicy::SkipIdenticalElseOverwrite => {
            if files_identical(src, dst).await {
                Decision::Skip
            } else {
                Decision::Write(dst.to_path_buf())
            }
        }
    }
}

/// Whether `src` and `dst` hold identical bytes. Cheap size gate first
/// (a size mismatch is an instant "different"), then a streamed
/// compare so we never hold two whole files in memory. Any I/O error
/// is treated as "not identical" so the caller falls back to its
/// overwrite/prompt branch rather than silently skipping a file it
/// couldn't verify.
async fn files_identical(src: &Path, dst: &Path) -> bool {
    use tokio::io::{AsyncReadExt, BufReader};

    let (sm, dm) = match tokio::try_join!(tokio::fs::metadata(src), tokio::fs::metadata(dst)) {
        Ok(pair) => pair,
        Err(_) => return false,
    };
    // Only regular files compare by content; a dir/file mismatch or a
    // size mismatch is definitively "different".
    if !sm.is_file() || !dm.is_file() || sm.len() != dm.len() {
        return false;
    }

    let (sf, df) = match tokio::try_join!(tokio::fs::File::open(src), tokio::fs::File::open(dst)) {
        Ok(pair) => pair,
        Err(_) => return false,
    };
    let mut sr = BufReader::with_capacity(CONTENT_COMPARE_CHUNK, sf);
    let mut dr = BufReader::with_capacity(CONTENT_COMPARE_CHUNK, df);
    let mut sbuf = vec![0u8; CONTENT_COMPARE_CHUNK];
    let mut dbuf = vec![0u8; CONTENT_COMPARE_CHUNK];
    loop {
        let sn = match sr.read(&mut sbuf).await {
            Ok(n) => n,
            Err(_) => return false,
        };
        if sn == 0 {
            return true; // both files are the same length → EOF together
        }
        // Fill the dst side to the same length before comparing, since
        // a single `read` may return a short buffer even mid-file.
        if let Err(()) = read_exact_n(&mut dr, &mut dbuf[..sn]).await {
            return false;
        }
        if sbuf[..sn] != dbuf[..sn] {
            return false;
        }
    }
}

/// Read exactly `buf.len()` bytes, returning `Err` on EOF-before-full
/// or any I/O error. (`AsyncReadExt::read_exact` errors on early EOF,
/// which is exactly the "different length despite equal metadata"
/// race we want to treat as not-identical.)
async fn read_exact_n<R: tokio::io::AsyncRead + Unpin>(
    r: &mut R,
    buf: &mut [u8],
) -> Result<(), ()> {
    use tokio::io::AsyncReadExt;
    r.read_exact(buf).await.map(|_| ()).map_err(|_| ())
}

/// Streamed content-compare chunk (64 KiB) — big enough to amortise
/// syscalls, small enough to stay off the stack and out of cache.
const CONTENT_COMPARE_CHUNK: usize = 64 * 1024;

async fn path_exists(p: &Path) -> bool {
    tokio::fs::symlink_metadata(p).await.is_ok()
}

async fn source_is_newer(src: &Path, dst: &Path) -> Option<bool> {
    let (sm, dm) = tokio::try_join!(tokio::fs::metadata(src), tokio::fs::metadata(dst)).ok()?;
    let st = sm.modified().ok()?;
    let dt = dm.modified().ok()?;
    Some(st > dt)
}

/// Produce a fresh destination path by appending `_2`, `_3`, ...
/// before the extension until a free slot is found. Starts at 2
/// because the original file is the implicit "1". Gives up at
/// 10_000.
///
/// The naming shape matches the user-facing convention: existing
/// file `foo.txt` plus a copy yields `foo_2.txt`, then `foo_3.txt`,
/// …. Folders (no extension) just get the suffix appended:
/// `Photos` → `Photos_2`.
async fn keep_both_path(dst: &Path) -> Option<PathBuf> {
    let parent = dst.parent()?;
    let stem = dst.file_stem()?.to_string_lossy().into_owned();
    let ext = dst
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    for n in 2..10_000 {
        let name = if ext.is_empty() {
            format!("{stem}_{n}")
        } else {
            format!("{stem}_{n}.{ext}")
        };
        let candidate = parent.join(&name);
        if !path_exists(&candidate).await {
            return Some(candidate);
        }
    }
    None
}

async fn prompt(src: &Path, dst: &Path, events: &mpsc::Sender<CopyEvent>) -> Decision {
    let (tx, rx) = oneshot::channel();
    let collision = Collision::new(src.to_path_buf(), dst.to_path_buf(), tx);
    if events.send(CopyEvent::Collision(collision)).await.is_err() {
        // No consumer — act like Skip rather than hang forever.
        return Decision::Skip;
    }
    match rx.await {
        Ok(CollisionResolution::Overwrite) => Decision::Write(dst.to_path_buf()),
        Ok(CollisionResolution::Rename(name)) => {
            let fresh = dst
                .parent()
                .map(|p| p.join(name))
                .unwrap_or_else(|| PathBuf::from("renamed"));
            Decision::Write(fresh)
        }
        Ok(CollisionResolution::Skip) | Err(_) => Decision::Skip,
        Ok(CollisionResolution::Abort) => Decision::Abort,
    }
}

#[cfg(test)]
mod content_aware_tests {
    use super::*;
    use tokio::sync::mpsc;

    fn sink() -> mpsc::Sender<CopyEvent> {
        // A receiver we immediately drop: any policy that would prompt
        // resolves to Skip (no consumer), which is what these tests
        // assert for the "else prompt" branch.
        let (tx, _rx) = mpsc::channel(1);
        tx
    }

    #[tokio::test]
    async fn files_identical_matches_only_equal_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        let c = dir.path().join("c.bin");
        std::fs::write(&a, vec![7u8; 200_000]).unwrap();
        std::fs::write(&b, vec![7u8; 200_000]).unwrap();
        let mut diff = vec![7u8; 200_000];
        diff[199_999] = 8;
        std::fs::write(&c, diff).unwrap();

        assert!(files_identical(&a, &b).await);
        assert!(!files_identical(&a, &c).await);
        // Size mismatch short-circuits to "different".
        std::fs::write(&c, vec![7u8; 199_999]).unwrap();
        assert!(!files_identical(&a, &c).await);
        // Missing file → not identical (never a silent skip).
        assert!(!files_identical(&a, &dir.path().join("nope")).await);
    }

    #[tokio::test]
    async fn skip_identical_else_overwrite_decisions() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.txt");
        let dst = dir.path().join("dst.txt");
        std::fs::write(&src, b"same").unwrap();
        std::fs::write(&dst, b"same").unwrap();
        assert_eq!(
            resolve(
                &CollisionPolicy::SkipIdenticalElseOverwrite,
                &src,
                &dst,
                &sink()
            )
            .await,
            Decision::Skip
        );

        std::fs::write(&dst, b"different").unwrap();
        assert_eq!(
            resolve(
                &CollisionPolicy::SkipIdenticalElseOverwrite,
                &src,
                &dst,
                &sink()
            )
            .await,
            Decision::Write(dst.clone())
        );
    }

    #[tokio::test]
    async fn skip_identical_else_prompt_decisions() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.txt");
        let dst = dir.path().join("dst.txt");
        std::fs::write(&src, b"same").unwrap();
        std::fs::write(&dst, b"same").unwrap();
        // Identical → skip, no prompt raised.
        assert_eq!(
            resolve(
                &CollisionPolicy::SkipIdenticalElsePrompt,
                &src,
                &dst,
                &sink()
            )
            .await,
            Decision::Skip
        );
        // Different → would prompt; with no consumer that resolves to Skip.
        std::fs::write(&dst, b"changed!!").unwrap();
        assert_eq!(
            resolve(
                &CollisionPolicy::SkipIdenticalElsePrompt,
                &src,
                &dst,
                &sink()
            )
            .await,
            Decision::Skip
        );
    }

    #[tokio::test]
    async fn nonexistent_dst_always_writes() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.txt");
        let dst = dir.path().join("dst.txt");
        std::fs::write(&src, b"data").unwrap();
        assert_eq!(
            resolve(
                &CollisionPolicy::SkipIdenticalElseOverwrite,
                &src,
                &dst,
                &sink()
            )
            .await,
            Decision::Write(dst.clone())
        );
    }
}
