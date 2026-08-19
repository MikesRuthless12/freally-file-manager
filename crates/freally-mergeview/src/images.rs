//! Phase 53 — image comparison: alpha blend and difference heatmap.

use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use serde::Serialize;

use crate::{MergeError, Result};

/// How much two images differ, in terms a person can act on.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageDiffStats {
    pub width: u32,
    pub height: u32,
    /// Pixels differing in any channel, alpha included.
    pub changed_pixels: u64,
    pub total_pixels: u64,
    /// `changed_pixels / total_pixels`, `0.0..=1.0`.
    pub changed_ratio: f32,
    /// Largest single-channel delta seen, `0..=255`. Separates "resaved
    /// at a different JPEG quality" (small) from "different picture"
    /// (large) even when the changed-pixel count is similar.
    pub max_channel_delta: u8,
    /// Mean absolute per-channel delta over changed pixels only.
    pub mean_channel_delta: f32,
}

fn decode(bytes: &[u8]) -> Result<DynamicImage> {
    image::load_from_memory(bytes).map_err(|e| MergeError::Decode(e.to_string()))
}

fn encode_png(img: &RgbaImage) -> Result<Vec<u8>> {
    let mut out = std::io::Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(img.clone())
        .write_to(&mut out, ImageFormat::Png)
        .map_err(|e| MergeError::Decode(e.to_string()))?;
    Ok(out.into_inner())
}

/// Both images decoded to RGBA at a common size.
///
/// Differently-sized images are compared over their overlap rather than
/// refused: a crop or a canvas extension is exactly the edit a merge
/// view exists to show, and rejecting it would leave the user with no
/// comparison at all. The stats report the overlap's dimensions.
fn decode_pair(a: &[u8], b: &[u8]) -> Result<(RgbaImage, RgbaImage, u32, u32)> {
    let ia = decode(a)?.to_rgba8();
    let ib = decode(b)?.to_rgba8();
    let w = ia.width().min(ib.width());
    let h = ia.height().min(ib.height());
    if w == 0 || h == 0 {
        return Err(MergeError::NotComparable(
            "one of the images has a zero-sized dimension".into(),
        ));
    }
    Ok((ia, ib, w, h))
}

/// Blend `a` and `b`, `alpha` being `b`'s weight (`0.0` = all `a`).
///
/// The classic "flip between the two" view, held at a midpoint so a
/// shifted element shows as a ghost.
pub fn blend(a: &[u8], b: &[u8], alpha: f32) -> Result<Vec<u8>> {
    let alpha = alpha.clamp(0.0, 1.0);
    let (ia, ib, w, h) = decode_pair(a, b)?;
    let mut out = RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let pa = ia.get_pixel(x, y).0;
            let pb = ib.get_pixel(x, y).0;
            let mut px = [0u8; 4];
            for c in 0..4 {
                let v = pa[c] as f32 * (1.0 - alpha) + pb[c] as f32 * alpha;
                px[c] = v.round().clamp(0.0, 255.0) as u8;
            }
            out.put_pixel(x, y, Rgba(px));
        }
    }
    encode_png(&out)
}

/// A heatmap of where the two differ, plus the stats behind it.
///
/// Unchanged pixels are dimmed to a grey underlay so the change is
/// legible in place rather than floating on black; changed pixels are
/// tinted by magnitude — yellow for slight, red for severe.
pub fn heatmap(a: &[u8], b: &[u8]) -> Result<(Vec<u8>, ImageDiffStats)> {
    let (ia, ib, w, h) = decode_pair(a, b)?;
    let mut out = RgbaImage::new(w, h);

    let mut changed: u64 = 0;
    let mut max_delta: u8 = 0;
    let mut delta_sum: u64 = 0;

    for y in 0..h {
        for x in 0..w {
            let pa = ia.get_pixel(x, y).0;
            let pb = ib.get_pixel(x, y).0;
            let mut worst = 0u8;
            let mut sum = 0u32;
            for c in 0..4 {
                let d = pa[c].abs_diff(pb[c]);
                worst = worst.max(d);
                sum += d as u32;
            }
            if worst == 0 {
                // Dimmed greyscale underlay: keeps the picture readable
                // without competing with the highlight colour.
                let luma = (pa[0] as u32 * 30 + pa[1] as u32 * 59 + pa[2] as u32 * 11) / 100;
                let g = (luma / 3) as u8;
                out.put_pixel(x, y, Rgba([g, g, g, 255]));
            } else {
                changed += 1;
                max_delta = max_delta.max(worst);
                delta_sum += (sum / 4) as u64;
                // Yellow → red by severity.
                let sev = worst;
                out.put_pixel(x, y, Rgba([255, 255u8.saturating_sub(sev), 0, 255]));
            }
        }
    }

    let total = u64::from(w) * u64::from(h);
    let stats = ImageDiffStats {
        width: w,
        height: h,
        changed_pixels: changed,
        total_pixels: total,
        changed_ratio: if total == 0 {
            0.0
        } else {
            changed as f32 / total as f32
        },
        max_channel_delta: max_delta,
        mean_channel_delta: if changed == 0 {
            0.0
        } else {
            delta_sum as f32 / changed as f32
        },
    };
    Ok((encode_png(&out)?, stats))
}

/// Stats without producing the heatmap image — for a summary line.
pub fn stats(a: &[u8], b: &[u8]) -> Result<ImageDiffStats> {
    heatmap(a, b).map(|(_, s)| s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32, px: [u8; 4]) -> Vec<u8> {
        let mut img = RgbaImage::new(w, h);
        for p in img.pixels_mut() {
            *p = Rgba(px);
        }
        encode_png(&img).unwrap()
    }

    #[test]
    fn identical_images_report_no_change() {
        let a = png(8, 8, [10, 20, 30, 255]);
        let s = stats(&a, &a).unwrap();
        assert_eq!(s.changed_pixels, 0);
        assert_eq!(s.changed_ratio, 0.0);
        assert_eq!(s.max_channel_delta, 0);
        assert_eq!(s.mean_channel_delta, 0.0);
    }

    #[test]
    fn a_wholly_different_image_reports_every_pixel() {
        let a = png(4, 4, [0, 0, 0, 255]);
        let b = png(4, 4, [255, 255, 255, 255]);
        let s = stats(&a, &b).unwrap();
        assert_eq!(s.changed_pixels, 16);
        assert_eq!(s.total_pixels, 16);
        assert_eq!(s.changed_ratio, 1.0);
        assert_eq!(s.max_channel_delta, 255);
    }

    #[test]
    fn a_slight_resave_is_distinguishable_from_a_new_picture() {
        // Both change every pixel, but magnitude tells them apart —
        // which is the difference between "recompressed" and "replaced".
        let base = png(4, 4, [100, 100, 100, 255]);
        let nudged = png(4, 4, [102, 100, 100, 255]);
        let replaced = png(4, 4, [0, 200, 30, 255]);

        let slight = stats(&base, &nudged).unwrap();
        let severe = stats(&base, &replaced).unwrap();
        assert_eq!(slight.changed_pixels, severe.changed_pixels);
        assert!(
            slight.max_channel_delta < severe.max_channel_delta,
            "magnitude must separate a resave from a replacement"
        );
    }

    #[test]
    fn alpha_only_changes_are_detected() {
        // A transparency edit leaves RGB identical; ignoring alpha would
        // report "no change" on a real edit.
        let a = png(4, 4, [10, 20, 30, 255]);
        let b = png(4, 4, [10, 20, 30, 0]);
        let s = stats(&a, &b).unwrap();
        assert_eq!(s.changed_pixels, 16);
        assert_eq!(s.max_channel_delta, 255);
    }

    #[test]
    fn different_sizes_compare_over_the_overlap() {
        // A crop must still produce a comparison rather than an error.
        let a = png(8, 8, [0, 0, 0, 255]);
        let b = png(4, 6, [0, 0, 0, 255]);
        let s = stats(&a, &b).unwrap();
        assert_eq!((s.width, s.height), (4, 6));
        assert_eq!(s.changed_pixels, 0, "the overlap is identical");
    }

    #[test]
    fn blend_endpoints_reproduce_each_input() {
        let a = png(2, 2, [0, 0, 0, 255]);
        let b = png(2, 2, [255, 255, 255, 255]);

        let at_zero = blend(&a, &b, 0.0).unwrap();
        assert_eq!(stats(&at_zero, &a).unwrap().changed_pixels, 0);

        let at_one = blend(&a, &b, 1.0).unwrap();
        assert_eq!(stats(&at_one, &b).unwrap().changed_pixels, 0);

        // Halfway is genuinely between, not either endpoint.
        let mid = blend(&a, &b, 0.5).unwrap();
        assert!(stats(&mid, &a).unwrap().changed_pixels > 0);
        assert!(stats(&mid, &b).unwrap().changed_pixels > 0);
    }

    #[test]
    fn blend_alpha_is_clamped_rather_than_wrapping() {
        let a = png(2, 2, [0, 0, 0, 255]);
        let b = png(2, 2, [255, 255, 255, 255]);
        // Out-of-range input must not produce garbage pixels.
        let low = blend(&a, &b, -5.0).unwrap();
        let high = blend(&a, &b, 9.0).unwrap();
        assert_eq!(stats(&low, &a).unwrap().changed_pixels, 0);
        assert_eq!(stats(&high, &b).unwrap().changed_pixels, 0);
    }

    #[test]
    fn undecodable_input_is_an_error_not_a_panic() {
        let a = png(2, 2, [0, 0, 0, 255]);
        assert!(matches!(
            stats(&a, b"definitely not an image"),
            Err(MergeError::Decode(_))
        ));
    }

    #[test]
    fn heatmap_is_a_real_png_of_the_overlap_size() {
        let a = png(6, 5, [0, 0, 0, 255]);
        let b = png(6, 5, [255, 0, 0, 255]);
        let (png_bytes, s) = heatmap(&a, &b).unwrap();
        assert_eq!(&png_bytes[..4], &[0x89, b'P', b'N', b'G']);
        let decoded = image::load_from_memory(&png_bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (s.width, s.height));
    }
}
