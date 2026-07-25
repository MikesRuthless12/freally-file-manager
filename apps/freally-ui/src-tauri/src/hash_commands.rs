//! FFM-M09 — hash inspector + clipboard compare.
//!
//! A standalone hashing surface: hand it any mix of files and folders
//! plus one of the eight [`HashAlgorithm`]s and it returns one digest
//! row per file (folders expand through the same tree walk the
//! checksum-sidecar jobs use). The panel then compares those digests
//! against a digest pasted from the clipboard, or against each other
//! when exactly two files were picked.
//!
//! The pasted-digest parsing is pure and unit-tested here rather than
//! in the frontend: users paste whatever their last tool printed —
//! a bare digest, a `sha256sum` line, a BSD-tagged line, an SFV row —
//! and all four shapes have to normalize to the same lowercase hex.

use std::path::PathBuf;

use serde::Serialize;

use freally_hash::HashAlgorithm;

use crate::ipc_safety::validate_ipc_path;

/// Upper bound on the rows one inspect run returns. A folder pick can
/// name an arbitrarily large tree; the panel is a comparison tool, not
/// a tree hasher (that is what the FFM-M08 sidecar jobs are for), so we
/// stop early and say so rather than streaming a million rows over IPC.
const MAX_ROWS: usize = 2000;

/// One hashed file.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HashRowDto {
    pub path: String,
    /// Lowercase hex digest. Empty when `error` is set.
    pub hex: String,
    pub size: u64,
    /// Why this row has no digest (unreadable file, walk failure, …).
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HashInspectReport {
    /// Canonical algorithm name the digests were produced with.
    pub algo: String,
    pub rows: Vec<HashRowDto>,
    /// `true` when the selection exceeded [`MAX_ROWS`] and the tail was
    /// dropped.
    pub truncated: bool,
}

/// Expand the picked roots into the concrete files to hash. A folder
/// contributes every file underneath it; a folder we cannot walk stays
/// in the list as itself so the hash attempt reports the real OS error
/// on its own row instead of vanishing.
fn expand_targets(roots: &[PathBuf]) -> (Vec<PathBuf>, bool) {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut truncated = false;
    for root in roots {
        if out.len() >= MAX_ROWS {
            truncated = true;
            break;
        }
        if !root.is_dir() {
            out.push(root.clone());
            continue;
        }
        match crate::sidecar_commands::collect_files(root) {
            Ok(rels) => {
                for rel in rels {
                    if out.len() >= MAX_ROWS {
                        truncated = true;
                        break;
                    }
                    out.push(root.join(rel));
                }
            }
            Err(_) => out.push(root.clone()),
        }
    }
    (out, truncated)
}

/// Hash every picked file (folders expanded) with `algo`.
#[tauri::command]
pub async fn hash_inspect(paths: Vec<String>, algo: String) -> Result<HashInspectReport, String> {
    use std::str::FromStr;
    let algo = HashAlgorithm::from_str(&algo).map_err(|_| format!("unknown algorithm: {algo}"))?;
    let mut roots = Vec::with_capacity(paths.len());
    for p in &paths {
        roots.push(validate_ipc_path(p).map_err(|e| e.to_string())?);
    }

    // The walk and every `stat` happen together on the blocking pool —
    // up to `MAX_ROWS` stats on the async runtime thread would stall
    // every other IPC command for the duration.
    let (targets, truncated) = tauri::async_runtime::spawn_blocking(move || {
        let (targets, truncated) = expand_targets(&roots);
        let sized: Vec<(PathBuf, u64)> = targets
            .into_iter()
            .map(|t| {
                let size = std::fs::metadata(&t).map(|m| m.len()).unwrap_or(0);
                (t, size)
            })
            .collect();
        (sized, truncated)
    })
    .await
    .map_err(|e| format!("hash inspect walk failed: {e}"))?;

    let mut rows = Vec::with_capacity(targets.len());
    for (target, size) in targets {
        let (hex, error) = match crate::hashing::hash_file_hex(&target, algo).await {
            Ok(hex) => (hex, None),
            Err(e) => (String::new(), Some(e)),
        };
        rows.push(HashRowDto {
            path: target.to_string_lossy().into_owned(),
            hex,
            size,
            error,
        });
    }

    Ok(HashInspectReport {
        algo: algo.name().to_string(),
        rows,
        truncated,
    })
}

/// A hex digest if `s` looks like one: even length, at least CRC32's
/// eight nibbles, nothing but hex digits.
fn as_hex(s: &str) -> Option<String> {
    let s = s.trim();
    if s.len() >= 8 && s.len() % 2 == 0 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(s.to_ascii_lowercase())
    } else {
        None
    }
}

/// Normalize whatever the user pasted into a lowercase hex digest.
///
/// Accepts, in order of preference: a bare digest, the tail of a
/// BSD-tagged sidecar line (`SHA256 (f.bin) = <hex>`), the leading
/// field of a GNU/coreutils line (`<hex>  f.bin`, `*`-prefix allowed),
/// and the trailing field of an SFV row (`f.bin <HEX>`). Returns `None`
/// when no candidate is hex-shaped.
///
/// The ordering only matters for pathological input — an SFV row whose
/// *filename* is itself bare hex would resolve to the name. Every real
/// shape is unambiguous.
pub fn parse_pasted_digest(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    if let Some(hex) = as_hex(line) {
        return Some(hex);
    }
    if let Some((_, tail)) = line.rsplit_once('=') {
        if let Some(hex) = as_hex(tail) {
            return Some(hex);
        }
    }
    let mut fields = line.split_whitespace();
    if let Some(hex) = fields
        .next()
        .and_then(|f| as_hex(f.trim_start_matches('*')))
    {
        return Some(hex);
    }
    line.split_whitespace().next_back().and_then(as_hex)
}

/// Parse a clipboard-pasted digest for the compare field. `None` means
/// "that isn't a digest" — the panel shows the hint instead of a verdict.
#[tauri::command]
pub fn hash_parse_digest(text: String) -> Option<String> {
    parse_pasted_digest(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_digest_is_lowercased() {
        let hex = "A".repeat(64);
        assert_eq!(parse_pasted_digest(&hex), Some("a".repeat(64)));
    }

    #[test]
    fn gnu_sidecar_line_yields_the_leading_digest() {
        assert_eq!(
            parse_pasted_digest("deadbeefdeadbeef  photo.jpg"),
            Some("deadbeefdeadbeef".to_string())
        );
        // coreutils binary-mode asterisk prefix on the path.
        assert_eq!(
            parse_pasted_digest("deadbeefdeadbeef *photo.jpg"),
            Some("deadbeefdeadbeef".to_string())
        );
    }

    #[test]
    fn bsd_tagged_line_yields_the_trailing_digest() {
        assert_eq!(
            parse_pasted_digest("SHA256 (f.bin) = ABCDEF0123456789"),
            Some("abcdef0123456789".to_string())
        );
    }

    #[test]
    fn sfv_row_yields_the_trailing_digest() {
        assert_eq!(
            parse_pasted_digest("game.iso DEADBEEF"),
            Some("deadbeef".to_string())
        );
    }

    #[test]
    fn surrounding_blank_lines_and_whitespace_are_ignored() {
        let pasted = "\n\n   abcdef0123456789   \n";
        assert_eq!(
            parse_pasted_digest(pasted),
            Some("abcdef0123456789".to_string())
        );
    }

    #[test]
    fn non_digest_text_is_rejected() {
        assert_eq!(parse_pasted_digest(""), None);
        assert_eq!(parse_pasted_digest("   \n  "), None);
        assert_eq!(parse_pasted_digest("not a digest at all"), None);
        // Too short to be any supported algorithm's digest.
        assert_eq!(parse_pasted_digest("abcdef"), None);
        // Odd length — a truncated paste, not a digest.
        assert_eq!(parse_pasted_digest("abcdef012"), None);
    }

    #[test]
    fn expand_targets_walks_folders_and_keeps_files() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(tree.join("sub")).unwrap();
        std::fs::write(tree.join("a.txt"), b"1").unwrap();
        std::fs::write(tree.join("sub").join("b.txt"), b"2").unwrap();
        let loose = dir.path().join("loose.bin");
        std::fs::write(&loose, b"3").unwrap();

        let (targets, truncated) = expand_targets(&[tree.clone(), loose.clone()]);
        assert!(!truncated);
        assert_eq!(targets.len(), 3);
        assert!(targets.contains(&tree.join("a.txt")));
        assert!(targets.contains(&tree.join("sub").join("b.txt")));
        assert!(targets.contains(&loose));
    }

    #[test]
    fn expand_targets_keeps_a_missing_root_so_its_error_surfaces() {
        let dir = tempfile::tempdir().unwrap();
        let ghost = dir.path().join("does-not-exist");
        let (targets, truncated) = expand_targets(std::slice::from_ref(&ghost));
        assert!(!truncated);
        assert_eq!(targets, vec![ghost]);
    }
}
