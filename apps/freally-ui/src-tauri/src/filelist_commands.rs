//! FFM-M13 — file-list import as a copy source.
//!
//! Accept a manifest of paths — TXT one-per-line, CSV, or JSON,
//! including the exports FFM-M07's failed-file ledger writes — and use
//! it as a job's source set instead of a single source root. The TOML
//! jobspec takes one root; this takes an arbitrary list.
//!
//! Two shapes of destination layout:
//!
//! - **flat** (no relative root): every file lands directly under the
//!   destination, keeping only its basename. This is what a plain
//!   multi-select drop already does.
//! - **structured** (relative root given): each file keeps its path
//!   *relative to that root*, so `R/a/b/c.txt` lands at
//!   `dst/a/b/c.txt`. The engine takes one destination root per job,
//!   so the planner groups the list by relative directory and the
//!   caller enqueues one job per group — the same shape FFM-M07's
//!   retry-failed pass uses.
//!
//! Parsing and planning are pure and unit-tested; the command layer
//! only does I/O.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::ipc_safety::validate_ipc_path;

/// Manifest text formats. Inferred from the file extension, so an
/// FFM-M07 export is consumable with no extra ceremony.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListFormat {
    /// One path per line; blank lines and `#` comments skipped.
    Txt,
    /// First column is the path. A leading `src`/`path` header row is
    /// dropped (FFM-M07 writes `src,dst,error_code,error_msg`).
    Csv,
    /// An array of strings, or an array of objects carrying a `src`
    /// (or `path`) key — the FFM-M07 JSON export shape.
    Json,
}

impl ListFormat {
    /// Infer from a manifest's extension; unknown extensions read as
    /// plain text, which is the forgiving choice for a hand-made list.
    pub fn from_path(p: &Path) -> Self {
        match p
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default()
            .as_str()
        {
            "csv" => Self::Csv,
            "json" => Self::Json,
            _ => Self::Txt,
        }
    }
}

/// Split one CSV record into fields, honouring RFC-4180 quoting.
fn csv_fields(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if in_quotes => {
                // A doubled quote inside a quoted field is one quote.
                if chars.peek() == Some(&'"') {
                    current.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            }
            '"' => in_quotes = true,
            ',' if !in_quotes => fields.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    fields.push(current);
    fields
}

/// Extract the path list from a manifest's text.
pub fn parse_list(text: &str, format: ListFormat) -> Result<Vec<String>, String> {
    match format {
        ListFormat::Txt => Ok(text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_string)
            .collect()),
        ListFormat::Csv => {
            let mut out = Vec::new();
            for (i, line) in text.lines().enumerate() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let first = csv_fields(line).into_iter().next().unwrap_or_default();
                let first = first.trim().to_string();
                if first.is_empty() {
                    continue;
                }
                // Drop the header row only when it looks like one.
                if i == 0 && matches!(first.as_str(), "src" | "path" | "source") {
                    continue;
                }
                out.push(first);
            }
            Ok(out)
        }
        ListFormat::Json => {
            let value: serde_json::Value = serde_json::from_str(text)
                .map_err(|e| format!("manifest is not valid JSON: {e}"))?;
            let array = value
                .as_array()
                .ok_or_else(|| "JSON manifest must be an array".to_string())?;
            let mut out = Vec::new();
            for entry in array {
                let picked = match entry {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Object(map) => map
                        .get("src")
                        .or_else(|| map.get("path"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    _ => None,
                };
                if let Some(p) = picked.filter(|p| !p.trim().is_empty()) {
                    out.push(p.trim().to_string());
                }
            }
            Ok(out)
        }
    }
}

/// One group of files that share a destination sub-directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportGroupDto {
    /// Destination sub-path, forward-slashed and possibly empty (the
    /// destination root itself). Never absolute, never contains `..`.
    pub rel_dir: String,
    pub sources: Vec<String>,
}

/// Group `paths` into the jobs an import should enqueue.
///
/// With no `relative_root`, every file goes into one group at the
/// destination root — the flat layout. With one, each file's parent
/// directory *relative to that root* becomes its group; a path outside
/// the root cannot keep a meaningful structure, so it falls back to the
/// flat group rather than being dropped.
///
/// Takes and returns `PathBuf`, not `String`: the CLI path
/// (`--files-from`) goes out of its way to keep non-UTF-8 filenames
/// byte-for-byte, and routing them through `to_string_lossy` here would
/// throw that away. The IPC surface converts at its own boundary, where
/// strings are genuinely the wire type.
pub fn plan_import(paths: &[PathBuf], relative_root: Option<&Path>) -> Vec<(String, Vec<PathBuf>)> {
    let mut groups: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for path in paths {
        let rel_dir = relative_root
            .and_then(|root| {
                path.parent()
                    .and_then(|parent| parent.strip_prefix(root).ok())
            })
            .map(|rel| rel.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        groups.entry(rel_dir).or_default().push(path.clone());
    }
    groups.into_iter().collect()
}

/// The deepest directory that contains every listed path — the default
/// the UI offers for "preserve structure relative to". `None` when the
/// list is empty or the paths share no common ancestor (different
/// drives).
pub fn common_root(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut iter = paths
        .iter()
        .map(|p| p.parent().map(Path::to_path_buf).unwrap_or_default());
    let mut root = iter.next()?;
    for candidate in iter {
        while !candidate.starts_with(&root) {
            if !root.pop() {
                return None;
            }
        }
    }
    if root.as_os_str().is_empty() {
        None
    } else {
        Some(root)
    }
}

/// What an import produced.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileListDto {
    /// Every listed path that exists on disk.
    pub paths: Vec<String>,
    /// Listed paths that could not be found — surfaced, never silently
    /// dropped, because a stale manifest is the common case.
    pub missing: Vec<String>,
    /// Suggested relative root (deepest common ancestor), if any.
    pub common_root: Option<String>,
}

/// Read and parse a manifest, splitting the entries into those that
/// exist and those that do not.
#[tauri::command]
pub async fn filelist_import(manifest: String) -> Result<FileListDto, String> {
    let manifest = validate_ipc_path(&manifest).map_err(|e| e.to_string())?;
    let text = tokio::fs::read_to_string(&manifest)
        .await
        .map_err(|e| format!("{}: {e}", manifest.display()))?;
    let entries = parse_list(&text, ListFormat::from_path(&manifest))?;

    let (paths, missing): (Vec<String>, Vec<String>) =
        tauri::async_runtime::spawn_blocking(move || {
            entries.into_iter().partition(|p| Path::new(p).exists())
        })
        .await
        .map_err(|e| format!("manifest scan failed: {e}"))?;

    let as_paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    let common_root = common_root(&as_paths).map(|p| p.to_string_lossy().into_owned());
    Ok(FileListDto {
        paths,
        missing,
        common_root,
    })
}

/// Group an imported list into the per-destination-subdirectory jobs
/// the caller should enqueue. `relative_root` empty means "flat".
#[tauri::command]
pub fn filelist_plan(
    paths: Vec<String>,
    relative_root: String,
) -> Result<Vec<ImportGroupDto>, String> {
    let root = if relative_root.trim().is_empty() {
        None
    } else {
        Some(validate_ipc_path(&relative_root).map_err(|e| e.to_string())?)
    };
    let as_paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    Ok(plan_import(&as_paths, root.as_deref())
        .into_iter()
        .map(|(rel_dir, sources)| ImportGroupDto {
            rel_dir,
            sources: sources
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn txt_skips_blanks_and_comments() {
        let text = "# a comment\n\n/a/one.txt\n  /b/two.txt  \n\n";
        let out = parse_list(text, ListFormat::Txt).unwrap();
        assert_eq!(out, vec!["/a/one.txt", "/b/two.txt"]);
    }

    #[test]
    fn csv_takes_the_first_column_and_drops_the_ffm_m07_header() {
        let text = "src,dst,error_code,error_msg\n\
                    \"C:\\a\\file, one.txt\",D:\\b\\x.txt,permission-denied,denied\n\
                    /plain/two.bin,/dst/two.bin,,\n";
        let out = parse_list(text, ListFormat::Csv).unwrap();
        assert_eq!(out, vec![r"C:\a\file, one.txt", "/plain/two.bin"]);
    }

    #[test]
    fn csv_without_a_header_keeps_every_row() {
        let out = parse_list("/a/one.txt\n/b/two.txt\n", ListFormat::Csv).unwrap();
        assert_eq!(out, vec!["/a/one.txt", "/b/two.txt"]);
    }

    #[test]
    fn json_accepts_a_bare_string_array() {
        let out = parse_list(r#"["/a/one.txt", "/b/two.txt"]"#, ListFormat::Json).unwrap();
        assert_eq!(out, vec!["/a/one.txt", "/b/two.txt"]);
    }

    #[test]
    fn json_accepts_the_ffm_m07_export_objects() {
        let text = r#"[
            {"src": "/a/one.txt", "dst": "/d/one.txt", "errorCode": "permission-denied"},
            {"path": "/b/two.txt"},
            {"unrelated": 1}
        ]"#;
        let out = parse_list(text, ListFormat::Json).unwrap();
        assert_eq!(out, vec!["/a/one.txt", "/b/two.txt"]);
    }

    #[test]
    fn json_rejects_a_non_array_document() {
        assert!(parse_list(r#"{"src": "/a"}"#, ListFormat::Json).is_err());
        assert!(parse_list("not json at all", ListFormat::Json).is_err());
    }

    #[test]
    fn format_is_inferred_from_the_extension() {
        assert_eq!(ListFormat::from_path(Path::new("l.CSV")), ListFormat::Csv);
        assert_eq!(ListFormat::from_path(Path::new("l.json")), ListFormat::Json);
        assert_eq!(ListFormat::from_path(Path::new("l.txt")), ListFormat::Txt);
        // Anything unfamiliar reads as a plain list.
        assert_eq!(ListFormat::from_path(Path::new("list")), ListFormat::Txt);
    }

    fn paths(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn a_flat_plan_is_one_group_at_the_destination_root() {
        let plan = plan_import(&paths(&["/r/a/one.txt", "/r/b/two.txt"]), None);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].0, "");
        assert_eq!(plan[0].1.len(), 2);
    }

    #[test]
    fn a_relative_root_preserves_structure_per_directory() {
        let plan = plan_import(
            &paths(&["/r/a/one.txt", "/r/a/two.txt", "/r/b/three.txt"]),
            Some(Path::new("/r")),
        );
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].0, "a");
        assert_eq!(plan[0].1.len(), 2);
        assert_eq!(plan[1].0, "b");
    }

    #[test]
    fn a_path_outside_the_relative_root_falls_back_to_flat() {
        let plan = plan_import(
            &paths(&["/r/a/one.txt", "/elsewhere/two.txt"]),
            Some(Path::new("/r")),
        );
        // "a" for the in-root file, "" for the stray — nothing dropped.
        assert_eq!(plan.len(), 2);
        let flat = plan.iter().find(|g| g.0.is_empty()).unwrap();
        assert_eq!(flat.1, paths(&["/elsewhere/two.txt"]));
    }

    #[test]
    fn a_file_directly_under_the_root_gets_the_empty_group() {
        let plan = plan_import(&paths(&["/r/top.txt"]), Some(Path::new("/r")));
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].0, "");
    }

    #[cfg(unix)]
    #[test]
    fn planning_preserves_non_utf8_paths_byte_for_byte() {
        // The CLI keeps non-UTF-8 filenames intact; the planner must not
        // launder them through `to_string_lossy` on the way to the queue.
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let raw = PathBuf::from(OsStr::from_bytes(b"/r/a/\xff\xfename.bin"));
        let plan = plan_import(std::slice::from_ref(&raw), Some(Path::new("/r")));
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].1, vec![raw]);
    }

    #[test]
    fn common_root_is_the_deepest_shared_ancestor() {
        assert_eq!(
            common_root(&paths(&[
                "/r/a/one.txt",
                "/r/a/b/two.txt",
                "/r/c/three.txt"
            ])),
            Some(PathBuf::from("/r"))
        );
    }

    #[test]
    fn common_root_of_one_file_is_its_own_directory() {
        assert_eq!(
            common_root(&paths(&["/r/a/one.txt"])),
            Some(PathBuf::from("/r/a"))
        );
    }

    #[test]
    fn common_root_is_none_for_an_empty_list() {
        assert_eq!(common_root(&[]), None);
    }
}
