//! Phase 53 — PSD comparison, layer by layer.
//!
//! A flattened image diff on a PSD answers "did the picture change";
//! it cannot answer "which layer changed", which is the question a
//! designer actually has. `psd` decodes the layer stack, so the diff
//! is per layer: added, removed, renamed, or repainted.

use psd::Psd;
use serde::Serialize;

use crate::{MergeError, Result};

/// One layer, as the merge view lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerInfo {
    pub name: String,
    pub visible: bool,
}

/// What happened to one layer between two revisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "layer")]
pub enum LayerChange {
    Added(String),
    Removed(String),
    /// Same name, different pixels.
    Repainted(String),
    /// Same name and pixels, but shown/hidden.
    VisibilityChanged(String),
    /// Present on both sides, but the layer's own geometry is such that
    /// decoding it would panic inside `psd`. Reported rather than
    /// guessed at, so the UI can say "this layer could not be compared"
    /// instead of silently claiming it is unchanged. See [`decodable`].
    Undecodable(String),
}

/// Document-level summary of a PSD.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PsdInfo {
    pub width: u32,
    pub height: u32,
    pub layers: Vec<LayerInfo>,
}

fn parse(bytes: &[u8]) -> Result<Psd> {
    Psd::from_bytes(bytes).map_err(|e| MergeError::WrongFormat {
        expected: "PSD",
        detail: e.to_string(),
    })
}

/// Read a PSD's dimensions and layer stack.
pub fn info(bytes: &[u8]) -> Result<PsdInfo> {
    let psd = parse(bytes)?;
    Ok(PsdInfo {
        width: psd.width(),
        height: psd.height(),
        layers: psd
            .layers()
            .iter()
            .map(|l| LayerInfo {
                name: l.name().to_string(),
                visible: l.visible(),
            })
            .collect(),
    })
}

/// Per-layer diff between two revisions of the same document.
///
/// Whether `psd`'s `PsdLayer::rgba()` can decode this layer without
/// panicking.
///
/// `rgba()` is not panic-safe on hostile or truncated input, and the
/// release profile sets `panic = "abort"`, so a bad layer would take the
/// whole app down rather than surfacing an error. Two of its three
/// failure modes are checkable from the public API:
///
/// * `rgba_idx` does `idx % layer.width()` — a zero-width (or
///   zero-height, which makes the buffer empty) layer divides by zero.
/// * `rgba_idx` does `top_in_psd.checked_mul(psd_width).unwrap()` — a
///   layer positioned far enough down overflows `i32`.
///
/// The third is **not** checkable: `red()` is `get_channel(Red).unwrap()`
/// and `channels` is private, so a layer that declares no red channel
/// still aborts. Closing that needs an upstream fix or a vendored
/// decoder; it is recorded in `docs/SECURITY_BACKLOG.md`.
fn decodable(psd_width: u32, layer: &psd::PsdLayer) -> bool {
    if layer.width() == 0 || layer.height() == 0 {
        return false;
    }
    // Highest row `rgba_idx` will reach, in PSD-space.
    let max_top = i64::from(layer.layer_top()) + i64::from(layer.height()) - 1;
    max_top
        .checked_mul(i64::from(psd_width))
        .is_some_and(|v| i32::try_from(v).is_ok())
}

/// Layers are matched by name. Photoshop allows duplicate names, so a
/// repeated name is matched positionally within its name group — which
/// is the best available identity when the format offers no stable id.
pub fn layer_diff(a: &[u8], b: &[u8]) -> Result<Vec<LayerChange>> {
    let pa = parse(a)?;
    let pb = parse(b)?;

    let mut changes = Vec::new();

    // Index by name, keeping order for duplicates.
    let mut left: std::collections::BTreeMap<String, Vec<usize>> = Default::default();
    for (i, l) in pa.layers().iter().enumerate() {
        left.entry(l.name().to_string()).or_default().push(i);
    }
    let mut right: std::collections::BTreeMap<String, Vec<usize>> = Default::default();
    for (i, l) in pb.layers().iter().enumerate() {
        right.entry(l.name().to_string()).or_default().push(i);
    }

    let names: std::collections::BTreeSet<&String> = left.keys().chain(right.keys()).collect();
    for name in names {
        let la = left.get(name).map(Vec::as_slice).unwrap_or(&[]);
        let lb = right.get(name).map(Vec::as_slice).unwrap_or(&[]);
        let common = la.len().min(lb.len());

        for k in 0..common {
            let layer_a = &pa.layers()[la[k]];
            let layer_b = &pb.layers()[lb[k]];
            // Geometry first: it is cheap, it is a real difference in
            // its own right, and it lets the common "layer was moved or
            // resized" case skip decoding entirely.
            let moved = layer_a.width() != layer_b.width()
                || layer_a.height() != layer_b.height()
                || layer_a.layer_left() != layer_b.layer_left()
                || layer_a.layer_top() != layer_b.layer_top();

            if moved {
                changes.push(LayerChange::Repainted(name.clone()));
            } else if !decodable(pa.width(), layer_a) || !decodable(pb.width(), layer_b) {
                // Refuse to decode a layer whose geometry would panic
                // inside `psd`. See `decodable`.
                changes.push(LayerChange::Undecodable(name.clone()));
            } else if layer_a.rgba() != layer_b.rgba() {
                // `rgba()` is the decoded layer surface; comparing it is
                // what distinguishes a repaint from a mere reorder.
                changes.push(LayerChange::Repainted(name.clone()));
            } else if layer_a.visible() != layer_b.visible() {
                changes.push(LayerChange::VisibilityChanged(name.clone()));
            }
        }
        for _ in common..la.len() {
            changes.push(LayerChange::Removed(name.clone()));
        }
        for _ in common..lb.len() {
            changes.push(LayerChange::Added(name.clone()));
        }
    }

    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_non_psd_is_rejected_with_the_expected_format_named() {
        let err = info(b"not a psd at all").unwrap_err();
        match err {
            MergeError::WrongFormat { expected, .. } => assert_eq!(expected, "PSD"),
            other => panic!("expected WrongFormat, got {other:?}"),
        }
    }

    #[test]
    fn layer_diff_on_two_non_psds_fails_rather_than_reporting_no_changes() {
        // Silently returning "no changes" for undecodable input would
        // read as "these files are identical", which is a lie.
        assert!(layer_diff(b"nope", b"nope").is_err());
    }

    #[test]
    fn layer_change_serialises_with_a_stable_tag() {
        // The frontend switches on `kind`, so the wire form is part of
        // the contract.
        let json = serde_json::to_string(&LayerChange::Repainted("Background".into())).unwrap();
        assert!(json.contains("\"kind\":\"repainted\""), "{json}");
        assert!(json.contains("\"layer\":\"Background\""), "{json}");
    }
}
