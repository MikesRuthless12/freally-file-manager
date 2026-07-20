//! FFM-M08 — checksum sidecar jobs (create + verify).
//!
//! Generate `.md5` / `.sha1` / `.sha256` / `.sha512` / `.b3` / `.sfv`
//! sidecars over a whole tree (GNU / coreutils style, or BSD-tagged
//! style), and verify a sidecar by re-hashing every file it references
//! relative to the sidecar's own directory. Both record a history job
//! so the drawer shows the run.
//!
//! The line formatting + extension→algorithm mapping are pure and
//! unit-tested; hashing goes through the shared `crate::hashing` helper.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::State;

use freally_hash::HashAlgorithm;
use freally_history::{ItemRow, JobSummary};

use crate::ipc_safety::validate_ipc_path;
use crate::state::AppState;

/// Sidecar text style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarStyle {
    /// `<hex>  <path>` — GNU coreutils / TeraCopy (`sha256sum -c`).
    Gnu,
    /// `ALGO (<path>) = <hex>` — BSD `md5 -r` / `shasum --tag`.
    Bsd,
    /// `<path> <HEX>` — SFV (CRC32 only), digest upper-cased.
    Sfv,
}

impl SidecarStyle {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "gnu" => Ok(Self::Gnu),
            "bsd" => Ok(Self::Bsd),
            "sfv" => Ok(Self::Sfv),
            other => Err(format!("unknown sidecar style: {other}")),
        }
    }
}

/// File extension a create job writes for `algo`. `.b3` for BLAKE3 (the
/// `b3sum` convention); `.sfv` is the CRC32 container.
pub fn sidecar_extension(algo: HashAlgorithm) -> &'static str {
    match algo {
        HashAlgorithm::Crc32 => "sfv",
        HashAlgorithm::Md5 => "md5",
        HashAlgorithm::Sha1 => "sha1",
        HashAlgorithm::Sha256 => "sha256",
        HashAlgorithm::Sha512 => "sha512",
        HashAlgorithm::Blake3 => "b3",
        HashAlgorithm::XxHash3_64 => "xxh64",
        HashAlgorithm::XxHash3_128 => "xxh128",
    }
}

/// Best-effort algorithm inference from a sidecar filename extension,
/// for verify jobs (drag-a-sidecar-onto-the-window).
pub fn algorithm_from_extension(ext: &str) -> Option<HashAlgorithm> {
    match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
        "sfv" => Some(HashAlgorithm::Crc32),
        "md5" => Some(HashAlgorithm::Md5),
        "sha1" => Some(HashAlgorithm::Sha1),
        "sha256" => Some(HashAlgorithm::Sha256),
        "sha512" => Some(HashAlgorithm::Sha512),
        "b3" | "blake3" => Some(HashAlgorithm::Blake3),
        "xxh64" => Some(HashAlgorithm::XxHash3_64),
        "xxh128" => Some(HashAlgorithm::XxHash3_128),
        _ => None,
    }
}

/// The BSD tag Explorer-facing tools print (`SHA256`, `MD5`, …).
fn bsd_tag(algo: HashAlgorithm) -> String {
    match algo {
        HashAlgorithm::Blake3 => "BLAKE3".to_string(),
        other => other.name().to_ascii_uppercase(),
    }
}

/// Forward-slash a relative path so a Windows-written sidecar verifies
/// on Linux (matches `freally_hash::sidecar`).
fn rel_display(p: &Path) -> String {
    let s = p.to_string_lossy();
    if cfg!(windows) {
        s.replace('\\', "/")
    } else {
        s.into_owned()
    }
}

/// Format one sidecar line for `(hex, relpath)` in `style`.
pub fn format_line(algo: HashAlgorithm, style: SidecarStyle, hex: &str, rel: &Path) -> String {
    let path = rel_display(rel);
    match style {
        SidecarStyle::Gnu => format!("{hex}  {path}"),
        SidecarStyle::Bsd => format!("{} ({}) = {}", bsd_tag(algo), path, hex),
        SidecarStyle::Sfv => format!("{} {}", path, hex.to_ascii_uppercase()),
    }
}

// ---------------------------------------------------------------------
// Reports
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SidecarCreateReport {
    pub files: u64,
    pub sidecar_path: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyLineDto {
    pub path: String,
    /// `"ok" | "failed" | "missing"`.
    pub status: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SidecarVerifyReport {
    pub ok: u64,
    pub failed: u64,
    pub missing: u64,
    /// The failing/missing lines (bounded), for the report UI.
    pub problems: Vec<VerifyLineDto>,
}

// ---------------------------------------------------------------------
// Tree walk
// ---------------------------------------------------------------------

/// Collect every regular file under `root` (recursively), returning
/// paths relative to `root`. `root` itself may be a single file.
fn collect_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if root.is_file() {
        if let Some(name) = root.file_name() {
            out.push(PathBuf::from(name));
        }
        return Ok(out);
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let ty = entry.file_type()?;
            if ty.is_dir() {
                stack.push(path);
            } else if ty.is_file() {
                if let Ok(rel) = path.strip_prefix(root) {
                    out.push(rel.to_path_buf());
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

// ---------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------

/// Create a checksum sidecar for the tree (or file) at `root`.
#[tauri::command]
pub async fn sidecar_create(
    root: String,
    algo: String,
    style: String,
    state: State<'_, AppState>,
) -> Result<SidecarCreateReport, String> {
    use std::str::FromStr;
    let root = validate_ipc_path(&root).map_err(|e| e.to_string())?;
    let algo = HashAlgorithm::from_str(&algo).map_err(|_| format!("unknown algorithm: {algo}"))?;
    let style = SidecarStyle::parse(&style)?;

    let base = root.clone();
    let mut rels = tauri::async_runtime::spawn_blocking(move || collect_files(&base))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{}: {e}", root.display()))?;

    // For a dir root the sidecar lands inside the tree; drop any prior
    // sidecar of the same name so a re-create never hashes it into
    // itself.
    let sidecar_path = sidecar_path_for(&root, algo);
    if let Ok(sidecar_rel) = sidecar_path.strip_prefix(&root) {
        rels.retain(|r| r != sidecar_rel);
    }

    // Anchor each relative entry at the tree root when hashing; a
    // single-file root anchors at its parent.
    let anchor = if root.is_file() {
        root.parent().map(Path::to_path_buf).unwrap_or_default()
    } else {
        root.clone()
    };

    let mut lines = Vec::with_capacity(rels.len());
    for rel in &rels {
        let hex = crate::hashing::hash_file_hex(&anchor.join(rel), algo).await?;
        lines.push(format_line(algo, style, &hex, rel));
    }

    let body = format!("{}\n", lines.join("\n"));
    tokio::fs::write(&sidecar_path, body)
        .await
        .map_err(|e| format!("{}: {e}", sidecar_path.display()))?;

    record_history(
        &state,
        "checksum",
        &root,
        &sidecar_path,
        rels.len() as u64,
        0,
    )
    .await;
    Ok(SidecarCreateReport {
        files: rels.len() as u64,
        sidecar_path: sidecar_path.to_string_lossy().into_owned(),
    })
}

/// Where a create job writes the sidecar. The sidecar's directory must
/// equal the directory the entries are relative to, so verify (which
/// anchors at `sidecar.parent()`) round-trips create:
/// - **dir root** — entries are relative to `root`, so the sidecar goes
///   *inside* the root (`root/<name>.<ext>`).
/// - **file root** — the single entry is relative to `root.parent()`,
///   so the sidecar sits beside the file (`parent/<name>.<ext>`).
fn sidecar_path_for(root: &Path, algo: HashAlgorithm) -> PathBuf {
    let ext = sidecar_extension(algo);
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "checksums".to_string());
    let dir = if root.is_dir() {
        root.to_path_buf()
    } else {
        root.parent().map(Path::to_path_buf).unwrap_or_default()
    };
    dir.join(format!("{name}.{ext}"))
}

/// Verify a sidecar by re-hashing every file it references, relative to
/// the sidecar's own directory.
#[tauri::command]
pub async fn sidecar_verify(
    sidecar: String,
    state: State<'_, AppState>,
) -> Result<SidecarVerifyReport, String> {
    let sidecar = validate_ipc_path(&sidecar).map_err(|e| e.to_string())?;
    let ext = sidecar
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    let algo = algorithm_from_extension(&ext).ok_or_else(|| {
        format!(
            "`{}` is not a recognized checksum sidecar",
            sidecar.display()
        )
    })?;
    let dir = sidecar.parent().map(Path::to_path_buf).unwrap_or_default();
    let contents = tokio::fs::read_to_string(&sidecar)
        .await
        .map_err(|e| format!("{}: {e}", sidecar.display()))?;
    let entries = freally_hash::sidecar::parse_sidecar(&contents);

    let mut report = SidecarVerifyReport::default();
    for entry in entries {
        let display = rel_display(&entry.relative_path);
        // The relpaths come from the (possibly hostile) sidecar file, so
        // apply the same job-root-relative guard the writer enforces — an
        // absolute path or a `..` segment would otherwise escape `dir` and
        // hash arbitrary files. Reject rather than join.
        if freally_hash::sidecar::validate_sidecar_relpath(&entry.relative_path).is_err() {
            report.failed += 1;
            push_problem(&mut report, display, "failed");
            continue;
        }
        let target = dir.join(&entry.relative_path);
        if !target.is_file() {
            report.missing += 1;
            push_problem(&mut report, display, "missing");
            continue;
        }
        match crate::hashing::hash_file_hex(&target, algo).await {
            Ok(hex) if hex.eq_ignore_ascii_case(&entry.hex_digest) => report.ok += 1,
            _ => {
                report.failed += 1;
                push_problem(&mut report, display, "failed");
            }
        }
    }

    record_history(
        &state,
        "verify",
        &dir,
        &sidecar,
        report.ok,
        report.failed + report.missing,
    )
    .await;
    Ok(report)
}

/// Keep the transported problem list bounded so a huge broken sidecar
/// can't blow up the IPC payload; the counts stay exact.
fn push_problem(report: &mut SidecarVerifyReport, path: String, status: &str) {
    const MAX_PROBLEMS: usize = 500;
    if report.problems.len() < MAX_PROBLEMS {
        report.problems.push(VerifyLineDto {
            path,
            status: status.to_string(),
        });
    }
}

async fn record_history(
    state: &AppState,
    kind: &str,
    src_root: &Path,
    dst: &Path,
    ok: u64,
    failed: u64,
) {
    let Some(history) = state.history.clone() else {
        return;
    };
    if let Ok(row) = history
        .record_start(&JobSummary {
            kind: kind.to_string(),
            status: "running".to_string(),
            src_root: src_root.to_path_buf(),
            dst_root: dst.to_path_buf(),
            ..Default::default()
        })
        .await
    {
        let _ = history
            .record_item(&ItemRow {
                job_row_id: row.0,
                src: src_root.to_path_buf(),
                dst: dst.to_path_buf(),
                size: 0,
                status: if failed == 0 { "ok" } else { "failed" }.to_string(),
                hash_hex: None,
                error_code: None,
                error_msg: None,
                timestamp_ms: 0,
            })
            .await;
        let status = if failed == 0 { "succeeded" } else { "failed" };
        let _ = history.record_finish(row, status, 0, ok, failed).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gnu_line_is_coreutils_shaped() {
        let line = format_line(
            HashAlgorithm::Sha256,
            SidecarStyle::Gnu,
            "abc123",
            Path::new("sub/file.txt"),
        );
        assert_eq!(line, "abc123  sub/file.txt");
    }

    #[test]
    fn bsd_line_is_tagged() {
        let line = format_line(
            HashAlgorithm::Sha256,
            SidecarStyle::Bsd,
            "abc123",
            Path::new("f.bin"),
        );
        assert_eq!(line, "SHA256 (f.bin) = abc123");
        let b3 = format_line(
            HashAlgorithm::Blake3,
            SidecarStyle::Bsd,
            "dd",
            Path::new("x"),
        );
        assert_eq!(b3, "BLAKE3 (x) = dd");
    }

    #[test]
    fn sfv_line_upper_cases_the_digest() {
        let line = format_line(
            HashAlgorithm::Crc32,
            SidecarStyle::Sfv,
            "deadbeef",
            Path::new("game.iso"),
        );
        assert_eq!(line, "game.iso DEADBEEF");
    }

    #[test]
    fn extension_round_trips_to_algorithm() {
        for algo in [
            HashAlgorithm::Md5,
            HashAlgorithm::Sha1,
            HashAlgorithm::Sha256,
            HashAlgorithm::Sha512,
            HashAlgorithm::Blake3,
            HashAlgorithm::Crc32,
        ] {
            let ext = sidecar_extension(algo);
            assert_eq!(algorithm_from_extension(ext), Some(algo), "ext {ext}");
        }
        assert_eq!(algorithm_from_extension(".txt"), None);
    }

    #[test]
    fn collect_files_walks_a_tree_relative_to_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("top.txt"), b"1").unwrap();
        std::fs::write(dir.path().join("a/mid.txt"), b"2").unwrap();
        std::fs::write(dir.path().join("a/b/deep.txt"), b"3").unwrap();
        let rels = collect_files(dir.path()).unwrap();
        assert_eq!(rels.len(), 3);
        assert!(rels.contains(&PathBuf::from("top.txt")));
        assert!(rels.contains(&PathBuf::from("a").join("b").join("deep.txt")));
    }

    #[test]
    fn single_file_root_yields_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("solo.bin");
        std::fs::write(&f, b"x").unwrap();
        assert_eq!(collect_files(&f).unwrap(), vec![PathBuf::from("solo.bin")]);
    }

    #[test]
    fn sidecar_lands_where_verify_will_anchor() {
        let dir = tempfile::tempdir().unwrap();
        // Dir root → sidecar INSIDE the root so verify's
        // `sidecar.parent()` == the anchor the entries are relative to.
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        let sc = sidecar_path_for(&tree, HashAlgorithm::Sha256);
        assert_eq!(sc.parent().unwrap(), tree);
        assert_eq!(sc.file_name().unwrap(), "tree.sha256");

        // File root → sidecar beside the file (its parent == anchor).
        let f = dir.path().join("solo.bin");
        std::fs::write(&f, b"x").unwrap();
        let sc = sidecar_path_for(&f, HashAlgorithm::Blake3);
        assert_eq!(sc.parent().unwrap(), dir.path());
        assert_eq!(sc.file_name().unwrap(), "solo.bin.b3");
    }

    #[test]
    fn xxhash_extensions_round_trip() {
        assert_eq!(
            algorithm_from_extension("xxh64"),
            Some(HashAlgorithm::XxHash3_64)
        );
        assert_eq!(
            algorithm_from_extension("xxh128"),
            Some(HashAlgorithm::XxHash3_128)
        );
    }
}
