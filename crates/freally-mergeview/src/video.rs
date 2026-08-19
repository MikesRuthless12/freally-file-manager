//! Phase 53 — video comparison, structural.
//!
//! **`mp4` is a demuxer, not a decoder.** It returns the box tree,
//! track parameters and the sample tables — never pixels. A decoded
//! thumbnail strip therefore needs ffmpeg, which is LGPL and a native
//! dependency this project has not taken on.
//!
//! What the demuxer can answer is still worth showing: duration,
//! dimensions, track layout, frame count, and the per-track byte
//! profile. A re-encode, a trim, a resolution change and an added
//! audio track all show up here — and nothing is presented as a frame
//! that is not one.

use serde::Serialize;

use crate::{MergeError, Result};

/// One track's parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackInfo {
    pub id: u32,
    pub kind: String,
    pub width: u16,
    pub height: u16,
    pub frame_count: u32,
}

/// Container-level summary.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoInfo {
    pub duration_secs: f64,
    pub timescale: u32,
    pub tracks: Vec<TrackInfo>,
    /// Total sample bytes per track, in track order — the shape a
    /// re-encode changes even when duration and dimensions do not.
    pub track_bytes: Vec<u64>,
}

/// How two clips differ, structurally.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoDiff {
    pub duration_delta_secs: f64,
    pub dimensions_changed: bool,
    pub track_count_changed: bool,
    /// Same duration and dimensions, different bytes — a re-encode.
    pub likely_reencode: bool,
}

/// Read a clip's structure.
pub fn info(bytes: &[u8]) -> Result<VideoInfo> {
    let size = bytes.len() as u64;
    let cursor = std::io::Cursor::new(bytes);
    let mut mp4 =
        mp4::Mp4Reader::read_header(cursor, size).map_err(|e| MergeError::WrongFormat {
            expected: "MP4",
            detail: e.to_string(),
        })?;

    let ids: Vec<u32> = mp4.tracks().keys().copied().collect();
    let mut tracks: Vec<TrackInfo> = Vec::new();
    let mut track_bytes: Vec<u64> = Vec::new();

    for id in ids {
        let (kind, width, height, count) = {
            let t = &mp4.tracks()[&id];
            (
                t.track_type()
                    .map(|k| format!("{k:?}"))
                    .unwrap_or_else(|_| "unknown".to_string()),
                t.width(),
                t.height(),
                t.sample_count(),
            )
        };
        tracks.push(TrackInfo {
            id,
            kind,
            width,
            height,
            frame_count: count,
        });
        // Sum the sample sizes: the byte profile that reveals a
        // re-encode at identical duration and dimensions.
        // `count` is `stsz.sample_count` read verbatim out of the
        // header — a corrupt or hostile clip can declare billions of
        // samples that the sample table does not actually describe.
        // Stop at the first sample the demuxer cannot produce rather
        // than spinning through the declared count: this runs on the
        // command thread, so a 4-billion-iteration loop is a hang.
        let mut total = 0u64;
        for i in 1..=count {
            match mp4.read_sample(id, i) {
                Ok(Some(s)) => total += s.bytes.len() as u64,
                _ => break,
            }
        }
        track_bytes.push(total);
    }
    tracks.sort_by_key(|t| t.id);

    Ok(VideoInfo {
        duration_secs: mp4.duration().as_secs_f64(),
        timescale: mp4.timescale(),
        tracks,
        track_bytes,
    })
}

/// Compare two clips structurally.
pub fn diff(a: &[u8], b: &[u8]) -> Result<VideoDiff> {
    let ia = info(a)?;
    let ib = info(b)?;

    let dims_a: Vec<(u16, u16)> = ia.tracks.iter().map(|t| (t.width, t.height)).collect();
    let dims_b: Vec<(u16, u16)> = ib.tracks.iter().map(|t| (t.width, t.height)).collect();
    let duration_delta = (ia.duration_secs - ib.duration_secs).abs();
    let dims_changed = dims_a != dims_b;
    let tracks_changed = ia.tracks.len() != ib.tracks.len();

    Ok(VideoDiff {
        duration_delta_secs: duration_delta,
        dimensions_changed: dims_changed,
        track_count_changed: tracks_changed,
        likely_reencode: duration_delta < 0.05
            && !dims_changed
            && !tracks_changed
            && ia.track_bytes != ib.track_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_non_mp4_is_rejected_with_the_format_named() {
        match info(b"not an mp4 file at all").unwrap_err() {
            MergeError::WrongFormat { expected, .. } => assert_eq!(expected, "MP4"),
            other => panic!("expected WrongFormat, got {other:?}"),
        }
    }

    #[test]
    fn diff_on_undecodable_input_errors_rather_than_claiming_equality() {
        assert!(diff(b"nope", b"nope").is_err());
    }

    #[test]
    fn video_diff_serialises_with_the_camel_case_the_frontend_expects() {
        let d = VideoDiff {
            duration_delta_secs: 0.0,
            dimensions_changed: false,
            track_count_changed: false,
            likely_reencode: true,
        };
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("\"likelyReencode\":true"), "{json}");
        assert!(json.contains("\"durationDeltaSecs\""), "{json}");
    }
}
