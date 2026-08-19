//! Phase 53 — 3-way merge previews for non-text files.
//!
//! A text merge shows three columns of lines. For a PNG, a PSD, a WAV,
//! an MP4 or a PDF there is no such view, so this crate produces a
//! *comparable rendering* per format that a merge UI can put side by
//! side: a blended image, a difference heatmap, a layer list, a
//! waveform, a sample timeline, a page summary.
//!
//! # What each format actually supports, and why
//!
//! The roadmap names one crate per format. Two of them are **parsers,
//! not renderers**, which bounds what is honestly possible:
//!
//! | Format | Crate | What we can produce |
//! |---|---|---|
//! | Image | `image` | Full decode → alpha blend, per-pixel heatmap, exact change stats |
//! | PSD | `psd` | Full decode → per-layer diff (added / removed / changed), flattened compare |
//! | Audio | `symphonia` | Full decode → peak/RMS waveform and a coarse band spectrogram |
//! | Video | `mp4` | Demux only → duration, tracks, dimensions, sample timeline. **No decoded frames** |
//! | PDF | `pdf` | Parse only → page count, per-page size/text extent. **No rasterised pages** |
//!
//! `mp4` is a demuxer: it hands back sample tables and codec
//! configuration, not pixels. `pdf` parses the document object graph,
//! not a rendered page. A thumbnail strip and a page-pair *image* view
//! therefore need a decoder (ffmpeg) and a rasteriser (pdfium/mupdf)
//! respectively — both large native dependencies that the roadmap does
//! not call for and that `cargo-deny`'s allowlist would have to be
//! reopened for. Until then those two formats get a structural diff,
//! which is genuinely useful (a re-encode, a page insertion, or a
//! duration change all show up) and is not dressed up as more than it
//! is.
//!
//! Anything unrecognised — Office documents especially — routes to
//! [`handoff`], which is the roadmap's own fallback.

pub mod audio;
pub mod ffmpeg;
pub mod handoff;
pub mod images;
pub mod pdfdoc;
pub mod psdfile;
pub mod video;

use std::path::Path;

use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MergeError {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),

    #[error("this file is not a {expected}: {detail}")]
    WrongFormat {
        expected: &'static str,
        detail: String,
    },

    #[error("decode failed: {0}")]
    Decode(String),

    #[error("the two files are not comparable: {0}")]
    NotComparable(String),
}

pub type Result<T> = std::result::Result<T, MergeError>;

/// Which preview pipeline a file belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergeFormat {
    Image,
    Psd,
    Audio,
    Video,
    Pdf,
    /// No preview — hand off to an external editor.
    Unsupported,
}

impl MergeFormat {
    /// Whether this format can render a visual comparison at all.
    pub fn has_preview(self) -> bool {
        !matches!(self, Self::Unsupported)
    }
}

/// Classify by content first, extension second.
///
/// Magic bytes win because a merge view that trusts a `.png` extension
/// on a JPEG renders nothing and blames the user. Extensions are the
/// fallback for containers whose magic is ambiguous or offset (MP4's
/// `ftyp` sits at byte 4, and plenty of audio has no signature at all).
pub fn detect(path: &Path, head: &[u8]) -> MergeFormat {
    if head.len() >= 8 {
        // PSD before image: `image` does not decode PSD, and a PSD's
        // `8BPS` signature is unambiguous.
        if &head[..4] == b"8BPS" {
            return MergeFormat::Psd;
        }
        if &head[..4] == b"%PDF" {
            return MergeFormat::Pdf;
        }
        if head.starts_with(&[0x89, b'P', b'N', b'G'])
            || head.starts_with(&[0xFF, 0xD8, 0xFF])
            || head.starts_with(b"GIF8")
            || head.starts_with(b"BM")
            || (head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP")
        {
            return MergeFormat::Image;
        }
        if head.len() >= 12 && &head[4..8] == b"ftyp" {
            // ISO-BMFF covers audio-only files too. `M4A `/`M4B `/`F4A `
            // is what iTunes and most AAC encoders stamp, and routing a
            // `.m4a` to the video pipeline yields a track table with
            // 0x0 dimensions and no waveform — the extension table
            // below already classes `m4a` as audio.
            return match &head[8..12] {
                b"M4A " | b"M4B " | b"F4A " => MergeFormat::Audio,
                _ => MergeFormat::Video,
            };
        }
        if head.starts_with(b"OggS") || head.starts_with(b"fLaC") || head.starts_with(b"ID3") {
            return MergeFormat::Audio;
        }
        // RIFF/WAVE is audio; RIFF/WEBP was caught above.
        if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WAVE" {
            return MergeFormat::Audio;
        }
    }

    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png" | "jpg" | "jpeg" | "gif" | "bmp" | "tif" | "tiff" | "webp") => {
            MergeFormat::Image
        }
        Some("psd" | "psb") => MergeFormat::Psd,
        Some("wav" | "mp3" | "flac" | "m4a" | "aac" | "ogg") => MergeFormat::Audio,
        Some("mp4" | "m4v" | "mov") => MergeFormat::Video,
        Some("pdf") => MergeFormat::Pdf,
        _ => MergeFormat::Unsupported,
    }
}

/// Read enough of a file to classify it, then classify.
pub fn detect_path(path: &Path) -> Result<MergeFormat> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut head = [0u8; 16];
    let n = f.read(&mut head)?;
    Ok(detect(path, &head[..n]))
}

/// A one-line verdict the merge UI shows above the panes.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeSummary {
    pub format: MergeFormat,
    /// `true` when the two inputs are byte-for-byte identical.
    pub identical: bool,
    /// Human-facing description of the difference.
    pub detail: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn magic_bytes_beat_a_lying_extension() {
        // A JPEG named .png must not be routed as a PNG-decode failure.
        let jpeg_head = [0xFF, 0xD8, 0xFF, 0xE0, 0, 0, 0, 0];
        assert_eq!(
            detect(&PathBuf::from("photo.png"), &jpeg_head),
            MergeFormat::Image
        );
        // A PSD named .png is a PSD.
        let psd_head = b"8BPS\0\x01\0\0";
        assert_eq!(
            detect(&PathBuf::from("art.png"), psd_head),
            MergeFormat::Psd
        );
    }

    #[test]
    fn riff_disambiguates_wav_from_webp() {
        let mut wav = Vec::from(*b"RIFF");
        wav.extend_from_slice(&[0, 0, 0, 0]);
        wav.extend_from_slice(b"WAVE");
        assert_eq!(detect(&PathBuf::from("a.bin"), &wav), MergeFormat::Audio);

        let mut webp = Vec::from(*b"RIFF");
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBP");
        assert_eq!(detect(&PathBuf::from("a.bin"), &webp), MergeFormat::Image);
    }

    #[test]
    fn mp4_ftyp_is_offset_by_four() {
        let mut head = vec![0, 0, 0, 0x20];
        head.extend_from_slice(b"ftypisom");
        head.extend_from_slice(&[0; 4]);
        assert_eq!(
            detect(&PathBuf::from("clip.bin"), &head),
            MergeFormat::Video
        );
    }

    #[test]
    fn extension_is_the_fallback_when_there_is_no_magic() {
        // A bare MP3 without an ID3 tag has no reliable signature.
        assert_eq!(
            detect(&PathBuf::from("song.mp3"), &[0u8; 4]),
            MergeFormat::Audio
        );
        assert_eq!(
            detect(&PathBuf::from("notes.docx"), &[0u8; 4]),
            MergeFormat::Unsupported
        );
    }

    #[test]
    fn unsupported_has_no_preview_and_everything_else_does() {
        assert!(!MergeFormat::Unsupported.has_preview());
        for f in [
            MergeFormat::Image,
            MergeFormat::Psd,
            MergeFormat::Audio,
            MergeFormat::Video,
            MergeFormat::Pdf,
        ] {
            assert!(f.has_preview(), "{f:?} should offer a preview");
        }
    }
}
