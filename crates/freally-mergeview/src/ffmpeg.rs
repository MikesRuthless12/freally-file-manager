//! Phase 53 — optional decoded video thumbnails via a *user-supplied*
//! ffmpeg.
//!
//! # Why this is a subprocess and not a library
//!
//! ffmpeg is LGPL-2.1 (GPL when built `--enable-gpl`). Linking it into
//! a closed-source app — even dynamically — means **distributing** it,
//! which drags in the obligation to ship its source or a written offer
//! and to let users relink. That is a real, permanent commitment on
//! every release, and `deny.toml` would have to be reopened to admit
//! LGPL at all.
//!
//! Invoking a binary the user installed themselves distributes
//! nothing. There is no linkage, no bundled blob outside `cargo-vet`,
//! no licence-policy change, and the arms-length process boundary means
//! even a GPL build is theirs to point us at. The cost is that the
//! feature is opt-in and absent by default — which is the honest
//! trade, and is why [`probe`] reports availability rather than
//! assuming it.
//!
//! Nothing here runs unless the user turns it on and ffmpeg is found.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::{MergeError, Result};

/// What we know about the user's ffmpeg, if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FfmpegStatus {
    pub available: bool,
    /// Resolved executable, when found.
    pub path: Option<PathBuf>,
    /// First line of `ffmpeg -version`, so the UI can show exactly
    /// which build is being used.
    pub version: Option<String>,
}

impl FfmpegStatus {
    fn missing() -> Self {
        Self {
            available: false,
            path: None,
            version: None,
        }
    }
}

/// Locate ffmpeg: an explicit path if configured, else `PATH`.
///
/// Never searches the app's own directory. On Windows a bare command
/// name resolves against the executable's directory *before* `PATH`,
/// so a hostile `ffmpeg.exe` dropped beside a portable install would
/// win — the same trap the scheduler's `os_tool` helper avoids.
pub fn probe(configured: Option<&Path>) -> FfmpegStatus {
    let candidate: PathBuf = match configured {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => match which_on_path() {
            Some(p) => p,
            None => return FfmpegStatus::missing(),
        },
    };

    let out = Command::new(&candidate).arg("-version").output();
    match out {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            FfmpegStatus {
                available: true,
                path: Some(candidate),
                version: text.lines().next().map(str::to_string),
            }
        }
        _ => FfmpegStatus::missing(),
    }
}

/// Resolve `ffmpeg` against `PATH` only, by absolute directory.
fn which_on_path() -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        // Skip relative entries: a `.` on PATH would reintroduce the
        // working-directory hijack this function exists to avoid.
        if !dir.is_absolute() {
            continue;
        }
        let candidate = dir.join(exe);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// The argument vector for a single-frame grab at `at_secs`.
///
/// Split out so the shape is testable without ffmpeg installed — the
/// ordering matters: `-ss` *before* `-i` seeks the input rather than
/// decoding to the timestamp, which is the difference between an
/// instant grab and reading the whole file.
pub fn thumbnail_args(input: &Path, at_secs: f64) -> Vec<String> {
    vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-nostdin".into(),
        "-ss".into(),
        format!("{at_secs:.3}"),
        "-i".into(),
        input.to_string_lossy().into_owned(),
        "-frames:v".into(),
        "1".into(),
        "-f".into(),
        "image2".into(),
        "-vcodec".into(),
        "png".into(),
        // Write to stdout so no temp file is left behind if we die.
        "-".into(),
    ]
}

/// Grab one PNG frame at `at_secs`.
pub fn thumbnail(ffmpeg: &Path, input: &Path, at_secs: f64) -> Result<Vec<u8>> {
    let out = Command::new(ffmpeg)
        .args(thumbnail_args(input, at_secs))
        .output()
        .map_err(MergeError::Io)?;
    if !out.status.success() {
        return Err(MergeError::Decode(format!(
            "ffmpeg exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    if out.stdout.is_empty() {
        return Err(MergeError::Decode(
            "ffmpeg produced no frame at that timestamp".into(),
        ));
    }
    Ok(out.stdout)
}

/// Evenly spaced frames across `duration_secs` — the thumbnail strip.
///
/// Samples are taken just inside the ends: a grab at exactly `0.0` or
/// exactly the duration frequently lands before the first keyframe or
/// past the last, and returns nothing.
pub fn thumbnail_strip(
    ffmpeg: &Path,
    input: &Path,
    duration_secs: f64,
    count: usize,
) -> Result<Vec<Vec<u8>>> {
    let count = count.max(1);
    let mut frames = Vec::with_capacity(count);
    for i in 0..count {
        let frac = (i as f64 + 0.5) / count as f64;
        frames.push(thumbnail(ffmpeg, input, duration_secs * frac)?);
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_ffmpeg_reports_unavailable_rather_than_erroring() {
        // The whole feature is optional; a machine without ffmpeg must
        // degrade quietly, not fail the merge view.
        let status = probe(Some(Path::new("/definitely/not/ffmpeg")));
        assert!(!status.available);
        assert!(status.path.is_none());
        assert!(status.version.is_none());
    }

    #[test]
    fn seek_comes_before_input_so_the_grab_is_instant() {
        let args = thumbnail_args(Path::new("/v/clip.mp4"), 12.5);
        let ss = args.iter().position(|a| a == "-ss").expect("-ss present");
        let i = args.iter().position(|a| a == "-i").expect("-i present");
        assert!(
            ss < i,
            "-ss must precede -i or ffmpeg decodes the whole file to reach the timestamp"
        );
        assert_eq!(args[ss + 1], "12.500");
    }

    #[test]
    fn output_goes_to_stdout_and_stdin_is_closed() {
        let args = thumbnail_args(Path::new("/v/clip.mp4"), 0.0);
        assert_eq!(args.last().unwrap(), "-", "frame must come back on stdout");
        // Without -nostdin ffmpeg can consume the parent's stdin and
        // wedge a non-interactive run.
        assert!(args.iter().any(|a| a == "-nostdin"));
        assert!(args.iter().any(|a| a == "-frames:v"));
    }

    #[test]
    fn the_input_path_is_one_argv_entry_so_spaces_are_safe() {
        let args = thumbnail_args(Path::new("/my videos/holiday clip.mp4"), 1.0);
        assert!(
            args.iter().any(|a| a == "/my videos/holiday clip.mp4"),
            "the path must not be split or quoted — there is no shell here"
        );
    }

    #[test]
    fn strip_samples_inside_the_ends() {
        // A grab at exactly 0.0 or exactly the duration usually returns
        // nothing; the midpoints of even slices avoid both edges.
        let mut seen = Vec::new();
        for i in 0..4 {
            seen.push((i as f64 + 0.5) / 4.0 * 10.0);
        }
        assert_eq!(seen, vec![1.25, 3.75, 6.25, 8.75]);
        assert!(seen.iter().all(|t| *t > 0.0 && *t < 10.0));
    }
}
