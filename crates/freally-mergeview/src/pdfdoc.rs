//! Phase 53 — PDF comparison, structural.
//!
//! **`pdf` parses, it does not rasterise.** Rendering a page to an
//! image needs pdfium or mupdf — a native library this project has not
//! taken on. What the parser gives is the page tree: page count,
//! per-page media box, and rotation. That answers "was a page inserted,
//! removed, resized or rotated", which is most of what a document diff
//! is asked for.

use serde::Serialize;

use crate::{MergeError, Result};

/// One page's geometry.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    pub index: usize,
    pub width: f32,
    pub height: f32,
    pub rotation: i32,
}

/// Document-level summary.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfInfo {
    pub page_count: usize,
    pub pages: Vec<PageInfo>,
}

/// What changed between two revisions.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfDiff {
    pub page_count_delta: i64,
    /// Indices whose geometry changed, over the common page range.
    pub resized_pages: Vec<usize>,
    pub rotated_pages: Vec<usize>,
}

/// Read a document's page geometry.
pub fn info(bytes: &[u8]) -> Result<PdfInfo> {
    let file =
        pdf::file::FileOptions::cached()
            .load(bytes)
            .map_err(|e| MergeError::WrongFormat {
                expected: "PDF",
                detail: e.to_string(),
            })?;

    let mut pages = Vec::new();
    for (index, page) in file.pages().enumerate() {
        let page = page.map_err(|e| MergeError::Decode(e.to_string()))?;
        // A page without its own MediaBox inherits one; a missing box
        // is reported as zero rather than guessed at.
        let (w, h) = page
            .media_box
            .map(|b| ((b.right - b.left).abs(), (b.top - b.bottom).abs()))
            .unwrap_or((0.0, 0.0));
        pages.push(PageInfo {
            index,
            width: w,
            height: h,
            rotation: page.rotate,
        });
    }

    Ok(PdfInfo {
        page_count: pages.len(),
        pages,
    })
}

/// Compare two documents.
pub fn diff(a: &[u8], b: &[u8]) -> Result<PdfDiff> {
    let ia = info(a)?;
    let ib = info(b)?;

    let common = ia.pages.len().min(ib.pages.len());
    let mut resized = Vec::new();
    let mut rotated = Vec::new();
    for i in 0..common {
        let (pa, pb) = (&ia.pages[i], &ib.pages[i]);
        // Page geometry is in points and carries float noise from a
        // round trip; a sub-point difference is not a resize.
        if (pa.width - pb.width).abs() > 0.5 || (pa.height - pb.height).abs() > 0.5 {
            resized.push(i);
        }
        if pa.rotation != pb.rotation {
            rotated.push(i);
        }
    }

    Ok(PdfDiff {
        page_count_delta: ib.page_count as i64 - ia.page_count as i64,
        resized_pages: resized,
        rotated_pages: rotated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_non_pdf_is_rejected_with_the_format_named() {
        match info(b"%NOTAPDF").unwrap_err() {
            MergeError::WrongFormat { expected, .. } => assert_eq!(expected, "PDF"),
            other => panic!("expected WrongFormat, got {other:?}"),
        }
    }

    #[test]
    fn diff_on_undecodable_input_errors_rather_than_claiming_equality() {
        assert!(diff(b"nope", b"nope").is_err());
    }

    #[test]
    fn pdf_diff_serialises_for_the_frontend() {
        let d = PdfDiff {
            page_count_delta: -2,
            resized_pages: vec![1],
            rotated_pages: vec![],
        };
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("\"pageCountDelta\":-2"), "{json}");
        assert!(json.contains("\"resizedPages\":[1]"), "{json}");
    }
}
