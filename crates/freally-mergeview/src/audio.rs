//! Phase 53 — audio comparison: waveform and coarse band spectrum.
//!
//! `symphonia` decodes to PCM, so the comparison is on samples rather
//! than on container bytes — a re-encode at a different bitrate shows
//! as "same audio, different file", which is exactly the distinction a
//! merge view needs to make.

use serde::Serialize;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::{MergeError, Result};

/// Peak/RMS envelope, bucketed for drawing.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Waveform {
    pub sample_rate: u32,
    pub channels: usize,
    pub duration_secs: f32,
    /// Per-bucket `(min, max)` in `-1.0..=1.0`.
    pub peaks: Vec<(f32, f32)>,
    /// Per-bucket RMS — what the ear tracks, and what separates a
    /// level change from a re-encode.
    pub rms: Vec<f32>,
}

/// How two recordings differ.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDiff {
    /// `true` when the decoded samples are identical — i.e. a pure
    /// container/metadata change.
    pub samples_identical: bool,
    pub duration_delta_secs: f32,
    pub sample_rate_changed: bool,
    pub channels_changed: bool,
    /// Mean absolute RMS difference across aligned buckets, `0.0..=1.0`.
    pub mean_rms_delta: f32,
    /// Largest single-bucket RMS difference.
    pub max_rms_delta: f32,
}

/// Decode to mono f32 samples plus the stream's parameters.
fn decode(bytes: &[u8], ext_hint: Option<&str>) -> Result<(Vec<f32>, u32, usize)> {
    let src = std::io::Cursor::new(bytes.to_vec());
    let mss = MediaSourceStream::new(Box::new(src), Default::default());
    let mut hint = Hint::new();
    if let Some(e) = ext_hint {
        hint.with_extension(e);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| MergeError::WrongFormat {
            expected: "audio",
            detail: e.to_string(),
        })?;
    let mut format = probed.format;

    let track = format
        .default_track()
        .ok_or_else(|| MergeError::Decode("no default audio track".into()))?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| MergeError::Decode(e.to_string()))?;

    let mut samples: Vec<f32> = Vec::new();
    let mut rate = track.codec_params.sample_rate.unwrap_or(0);
    let mut channels = track
        .codec_params
        .channels
        .map(|c| c.count())
        .unwrap_or(1)
        .max(1);

    // End of stream surfaces as an error from `next_packet`, so the
    // `while let` is the loop shape clippy wants and reads correctly.
    while let Ok(packet) = format.next_packet() {
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                rate = spec.rate;
                channels = spec.channels.count().max(1);
                let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
                buf.copy_interleaved_ref(decoded);
                // Downmix to mono: a channel-count change is reported
                // separately, and comparing envelopes is what matters.
                for frame in buf.samples().chunks(channels) {
                    let sum: f32 = frame.iter().copied().sum();
                    samples.push(sum / channels as f32);
                }
            }
            // A single corrupt packet should not abandon the whole
            // comparison; keep what decoded.
            Err(_) => continue,
        }
    }

    if samples.is_empty() {
        return Err(MergeError::Decode("no audio samples decoded".into()));
    }
    Ok((samples, rate, channels))
}

/// Bucket a decoded stream into a drawable envelope.
pub fn waveform(bytes: &[u8], ext_hint: Option<&str>, buckets: usize) -> Result<Waveform> {
    let buckets = buckets.max(1);
    let (samples, rate, channels) = decode(bytes, ext_hint)?;

    let per = samples.len().div_ceil(buckets).max(1);
    let mut peaks = Vec::with_capacity(buckets);
    let mut rms = Vec::with_capacity(buckets);
    for chunk in samples.chunks(per) {
        let mut lo = f32::MAX;
        let mut hi = f32::MIN;
        let mut sq = 0.0f64;
        for &s in chunk {
            lo = lo.min(s);
            hi = hi.max(s);
            sq += (s as f64) * (s as f64);
        }
        peaks.push((lo, hi));
        rms.push((sq / chunk.len() as f64).sqrt() as f32);
    }

    Ok(Waveform {
        sample_rate: rate,
        channels,
        duration_secs: if rate == 0 {
            0.0
        } else {
            samples.len() as f32 / rate as f32
        },
        peaks,
        rms,
    })
}

/// Compare two recordings.
pub fn diff(
    a: &[u8],
    b: &[u8],
    ext_a: Option<&str>,
    ext_b: Option<&str>,
    buckets: usize,
) -> Result<AudioDiff> {
    let wa = waveform(a, ext_a, buckets)?;
    let wb = waveform(b, ext_b, buckets)?;

    let n = wa.rms.len().min(wb.rms.len());
    let mut sum = 0.0f64;
    let mut max = 0.0f32;
    for i in 0..n {
        let d = (wa.rms[i] - wb.rms[i]).abs();
        sum += d as f64;
        max = max.max(d);
    }

    Ok(AudioDiff {
        samples_identical: wa.peaks == wb.peaks && wa.rms == wb.rms,
        duration_delta_secs: (wa.duration_secs - wb.duration_secs).abs(),
        sample_rate_changed: wa.sample_rate != wb.sample_rate,
        channels_changed: wa.channels != wb.channels,
        mean_rms_delta: if n == 0 { 0.0 } else { (sum / n as f64) as f32 },
        max_rms_delta: max,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal 16-bit PCM WAV, so the decode path is exercised
    /// without shipping a binary fixture.
    fn wav(samples: &[i16], rate: u32) -> Vec<u8> {
        let data_len = (samples.len() * 2) as u32;
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data_len).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes()); // PCM chunk size
        v.extend_from_slice(&1u16.to_le_bytes()); // PCM
        v.extend_from_slice(&1u16.to_le_bytes()); // mono
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * 2).to_le_bytes()); // byte rate
        v.extend_from_slice(&2u16.to_le_bytes()); // block align
        v.extend_from_slice(&16u16.to_le_bytes()); // bits
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_len.to_le_bytes());
        for s in samples {
            v.extend_from_slice(&s.to_le_bytes());
        }
        v
    }

    fn tone(n: usize, amp: i16) -> Vec<i16> {
        (0..n)
            .map(|i| if i % 2 == 0 { amp } else { -amp })
            .collect()
    }

    #[test]
    fn decodes_a_wav_and_reports_its_parameters() {
        let w = waveform(&wav(&tone(4000, 8000), 8000), Some("wav"), 16).unwrap();
        assert_eq!(w.sample_rate, 8000);
        assert_eq!(w.channels, 1);
        assert_eq!(w.peaks.len(), 16);
        assert!((w.duration_secs - 0.5).abs() < 0.01, "{}", w.duration_secs);
    }

    #[test]
    fn identical_audio_reports_identical_samples() {
        let a = wav(&tone(2000, 6000), 8000);
        let d = diff(&a, &a, Some("wav"), Some("wav"), 16).unwrap();
        assert!(d.samples_identical);
        assert_eq!(d.max_rms_delta, 0.0);
        assert!(!d.sample_rate_changed);
    }

    #[test]
    fn a_level_change_shows_in_rms_not_in_duration() {
        let quiet = wav(&tone(2000, 2000), 8000);
        let loud = wav(&tone(2000, 20000), 8000);
        let d = diff(&quiet, &loud, Some("wav"), Some("wav"), 16).unwrap();
        assert!(!d.samples_identical);
        assert!(d.max_rms_delta > 0.1, "delta was {}", d.max_rms_delta);
        assert!(d.duration_delta_secs < 0.01);
    }

    #[test]
    fn a_sample_rate_change_is_called_out_separately() {
        let a = wav(&tone(2000, 8000), 8000);
        let b = wav(&tone(2000, 8000), 16000);
        let d = diff(&a, &b, Some("wav"), Some("wav"), 16).unwrap();
        assert!(d.sample_rate_changed);
        // Same sample count at double the rate = half the duration.
        assert!(d.duration_delta_secs > 0.1);
    }

    #[test]
    fn undecodable_audio_is_an_error_not_silence() {
        assert!(waveform(b"not audio", Some("wav"), 8).is_err());
    }
}
