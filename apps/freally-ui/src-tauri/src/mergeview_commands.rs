//! Phase 53 — 3-way merge previews for non-text files.
//!
//! Thin IPC over [`freally_mergeview`]. Every path argument goes
//! through the Phase 17e gate, and images come back as base64 data
//! URLs because the webview cannot read arbitrary disk paths.

use std::path::PathBuf;

use base64::Engine as _;
use serde::Serialize;

use crate::ipc_safety::validate_ipc_path;
use crate::state::AppState;

/// Upper bound on the waveform bucket count a caller may ask for.
/// Wider than any strip the UI draws; the point is that the number is
/// bounded at all, because it becomes a `Vec::with_capacity`.
const MAX_WAVEFORM_BUCKETS: usize = 4096;

/// Upper bound on decoded video frames per request. Each one is a
/// separate `ffmpeg` process, so this is a wall-clock guard as much as
/// an allocation guard.
const MAX_THUMBNAIL_FRAMES: usize = 64;

/// What kind of preview a path supports, and why.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeFormatDto {
    /// `image` | `psd` | `audio` | `video` | `pdf` | `unsupported`.
    pub format: String,
    pub has_preview: bool,
}

/// Phase 17e — the single filesystem entry point for this module.
/// Every wire path runs through [`validate_ipc_path`] here before it
/// reaches the disk, so the commands below never touch a raw `String`.
/// The name is load-bearing: `phase_17e_ipc_audit` recognises it as a
/// delegating gate.
fn read_gated(path: &str) -> Result<(PathBuf, Vec<u8>), String> {
    let p = validate_ipc_path(path).map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&p).map_err(|e| format!("read {}: {e}", p.display()))?;
    Ok((p, bytes))
}

fn data_url(png: &[u8]) -> String {
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    )
}

fn ext_of(p: &std::path::Path) -> Option<String> {
    p.extension().and_then(|e| e.to_str()).map(str::to_string)
}

/// `merge_detect(path)` — which preview pipeline this file belongs to.
#[tauri::command]
pub fn merge_detect(path: String) -> Result<MergeFormatDto, String> {
    let (p, bytes) = read_gated(&path)?;
    let head = &bytes[..bytes.len().min(16)];
    let f = freally_mergeview::detect(&p, head);
    Ok(MergeFormatDto {
        format: match f {
            freally_mergeview::MergeFormat::Image => "image",
            freally_mergeview::MergeFormat::Psd => "psd",
            freally_mergeview::MergeFormat::Audio => "audio",
            freally_mergeview::MergeFormat::Video => "video",
            freally_mergeview::MergeFormat::Pdf => "pdf",
            freally_mergeview::MergeFormat::Unsupported => "unsupported",
        }
        .to_string(),
        has_preview: f.has_preview(),
    })
}

/// Image comparison: heatmap PNG + stats.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageCompareDto {
    /// `data:image/png;base64,…`
    pub heatmap: String,
    pub stats: freally_mergeview::images::ImageDiffStats,
}

/// `merge_image_compare(a, b)` — difference heatmap and stats.
#[tauri::command]
pub fn merge_image_compare(a: String, b: String) -> Result<ImageCompareDto, String> {
    let (_, ba) = read_gated(&a)?;
    let (_, bb) = read_gated(&b)?;
    let (png, stats) = freally_mergeview::images::heatmap(&ba, &bb).map_err(|e| e.to_string())?;
    Ok(ImageCompareDto {
        heatmap: data_url(&png),
        stats,
    })
}

/// `merge_image_blend(a, b, alpha)` — the flip/ghost view.
#[tauri::command]
pub fn merge_image_blend(a: String, b: String, alpha: f32) -> Result<String, String> {
    let (_, ba) = read_gated(&a)?;
    let (_, bb) = read_gated(&b)?;
    let png = freally_mergeview::images::blend(&ba, &bb, alpha).map_err(|e| e.to_string())?;
    Ok(data_url(&png))
}

/// `merge_psd_layers(a, b)` — per-layer diff.
#[tauri::command]
pub fn merge_psd_layers(
    a: String,
    b: String,
) -> Result<Vec<freally_mergeview::psdfile::LayerChange>, String> {
    let (_, ba) = read_gated(&a)?;
    let (_, bb) = read_gated(&b)?;
    freally_mergeview::psdfile::layer_diff(&ba, &bb).map_err(|e| e.to_string())
}

/// `merge_audio_compare(a, b, buckets)` — waveform envelopes + verdict.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioCompareDto {
    pub left: freally_mergeview::audio::Waveform,
    pub right: freally_mergeview::audio::Waveform,
    pub diff: freally_mergeview::audio::AudioDiff,
}

#[tauri::command]
pub fn merge_audio_compare(
    a: String,
    b: String,
    buckets: usize,
) -> Result<AudioCompareDto, String> {
    // `waveform` does `Vec::with_capacity(buckets)`, and a failed
    // allocation aborts the process rather than unwinding, so an
    // absurd bucket count off the wire would kill the app outright.
    // No waveform strip is wider than a few thousand pixels.
    let buckets = buckets.clamp(1, MAX_WAVEFORM_BUCKETS);
    let (pa, ba) = read_gated(&a)?;
    let (pb, bb) = read_gated(&b)?;
    let ea = ext_of(&pa);
    let eb = ext_of(&pb);
    Ok(AudioCompareDto {
        left: freally_mergeview::audio::waveform(&ba, ea.as_deref(), buckets)
            .map_err(|e| e.to_string())?,
        right: freally_mergeview::audio::waveform(&bb, eb.as_deref(), buckets)
            .map_err(|e| e.to_string())?,
        diff: freally_mergeview::audio::diff(&ba, &bb, ea.as_deref(), eb.as_deref(), buckets)
            .map_err(|e| e.to_string())?,
    })
}

/// `merge_video_compare(a, b)` — structural clip diff.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoCompareDto {
    pub left: freally_mergeview::video::VideoInfo,
    pub right: freally_mergeview::video::VideoInfo,
    pub diff: freally_mergeview::video::VideoDiff,
}

#[tauri::command]
pub fn merge_video_compare(a: String, b: String) -> Result<VideoCompareDto, String> {
    let (_, ba) = read_gated(&a)?;
    let (_, bb) = read_gated(&b)?;
    Ok(VideoCompareDto {
        left: freally_mergeview::video::info(&ba).map_err(|e| e.to_string())?,
        right: freally_mergeview::video::info(&bb).map_err(|e| e.to_string())?,
        diff: freally_mergeview::video::diff(&ba, &bb).map_err(|e| e.to_string())?,
    })
}

/// `merge_pdf_compare(a, b)` — structural document diff.
#[tauri::command]
pub fn merge_pdf_compare(
    a: String,
    b: String,
) -> Result<freally_mergeview::pdfdoc::PdfDiff, String> {
    let (_, ba) = read_gated(&a)?;
    let (_, bb) = read_gated(&b)?;
    freally_mergeview::pdfdoc::diff(&ba, &bb).map_err(|e| e.to_string())
}

/// `merge_ffmpeg_status()` — whether decoded video thumbnails are
/// available on this machine.
///
/// ffmpeg is never bundled: this reports on a binary the user
/// installed themselves, which is what keeps its LGPL terms off this
/// project entirely.
///
/// Reports unavailable without probing while the feature is off.
/// `probe` *runs* the candidate (`ffmpeg -version`), so probing on a
/// disabled feature would execute a binary the user has not opted into
/// — and the module doc's "nothing here runs unless the user turns it
/// on" would simply be untrue. `merge_video_thumbnails` already gates
/// this way; this is the reporting half of the same rule. The Settings
/// pane re-probes right after saving prefs, so enabling the toggle
/// still shows the resolved path immediately.
#[tauri::command]
pub fn merge_ffmpeg_status(
    state: tauri::State<'_, AppState>,
) -> freally_mergeview::ffmpeg::FfmpegStatus {
    let merge = state.settings_snapshot().merge;
    if !merge.ffmpeg_enabled {
        return freally_mergeview::ffmpeg::FfmpegStatus {
            available: false,
            path: None,
            version: None,
        };
    }
    let path = (!merge.ffmpeg_path.is_empty()).then(|| PathBuf::from(merge.ffmpeg_path));
    freally_mergeview::ffmpeg::probe(path.as_deref())
}

/// `merge_video_thumbnails(path, count)` — decoded frames, when the
/// user has opted in and ffmpeg is present.
#[tauri::command]
pub fn merge_video_thumbnails(
    state: tauri::State<'_, AppState>,
    path: String,
    count: usize,
) -> Result<Vec<String>, String> {
    let settings = state.settings_snapshot().merge;
    if !settings.ffmpeg_enabled {
        return Err("merge-ffmpeg-disabled".to_string());
    }
    // Each frame is a separate `ffmpeg` invocation and the result Vec
    // is pre-allocated, so an unbounded count off the wire is both a
    // freeze and an aborting allocation. A strip is a strip.
    let count = count.clamp(1, MAX_THUMBNAIL_FRAMES);
    let configured =
        (!settings.ffmpeg_path.is_empty()).then(|| PathBuf::from(settings.ffmpeg_path));
    let status = freally_mergeview::ffmpeg::probe(configured.as_deref());
    let Some(exe) = status.path else {
        return Err("merge-ffmpeg-missing".to_string());
    };

    let (p, bytes) = read_gated(&path)?;
    // Duration comes from the demuxer, so the strip is spread across
    // the real clip rather than a guess.
    let info = freally_mergeview::video::info(&bytes).map_err(|e| e.to_string())?;
    let frames = freally_mergeview::ffmpeg::thumbnail_strip(&exe, &p, info.duration_secs, count)
        .map_err(|e| e.to_string())?;
    Ok(frames.iter().map(|f| data_url(f)).collect())
}

/// `merge_handoff(target, base, local, remote)` — write the three
/// versions out for an external editor.
#[tauri::command]
pub fn merge_handoff(
    target: String,
    base: String,
    local: String,
    remote: String,
) -> Result<freally_mergeview::handoff::HandoffSet, String> {
    let t = validate_ipc_path(&target).map_err(|e| e.to_string())?;
    let (_, b) = read_gated(&base)?;
    let (_, l) = read_gated(&local)?;
    let (_, r) = read_gated(&remote)?;
    freally_mergeview::handoff::write_set(&t, &b, &l, &r).map_err(|e| e.to_string())
}

/// The ffmpeg opt-in, as the Settings pane edits it.
#[derive(Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeFfmpegPrefsDto {
    pub enabled: bool,
    /// Empty = search PATH.
    pub path: String,
}

/// `merge_ffmpeg_prefs_get()` — read the opt-in.
#[tauri::command]
pub fn merge_ffmpeg_prefs_get(state: tauri::State<'_, AppState>) -> MergeFfmpegPrefsDto {
    let m = state.settings_snapshot().merge;
    MergeFfmpegPrefsDto {
        enabled: m.ffmpeg_enabled,
        path: m.ffmpeg_path,
    }
}

/// `merge_ffmpeg_prefs_set(prefs)` — persist the opt-in.
///
/// Enabling this without ffmpeg installed is deliberately not an
/// error: the merge view falls back to the structural diff, and the
/// status probe tells the user what is actually available.
#[tauri::command]
pub fn merge_ffmpeg_prefs_set(
    state: tauri::State<'_, AppState>,
    prefs: MergeFfmpegPrefsDto,
) -> Result<(), String> {
    // Phase 17e — this string is later handed to `Command::new`, so it
    // is the one path argument in this module that does not reach the
    // disk through `read_gated`. Gate it here instead of storing an
    // arbitrary wire string as an executable to run. Empty stays empty
    // (= "search PATH", which `ffmpeg::probe` hardens separately).
    let ffmpeg_path = if prefs.path.is_empty() {
        String::new()
    } else {
        validate_ipc_path(&prefs.path)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .into_owned()
    };
    let mut guard = state.settings.write().map_err(|e| e.to_string())?;
    guard.merge.ffmpeg_enabled = prefs.enabled;
    guard.merge.ffmpeg_path = ffmpeg_path;
    let path = state.settings_path.as_path();
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    guard
        .save_to(path)
        .map_err(|e| format!("save settings: {e}"))
}
