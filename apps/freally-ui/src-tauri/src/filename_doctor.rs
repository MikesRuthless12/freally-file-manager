//! FFM-M12 — cross-filesystem filename doctor.
//!
//! Preflight scan for names the *destination* cannot hold: Windows
//! reserved device names, characters the target filesystem rejects,
//! trailing dots and spaces (Windows silently strips them), component
//! and path length overflow, and case-fold collisions — two files that
//! coexist on a case-sensitive source but land on the same name on
//! NTFS or a default APFS volume.
//!
//! New delta vs Phase 30 (`freally_core::translate`): that module
//! ships Unicode NFC/NFD normalization — *encoding*. This is
//! *legality* and *collision*, and it reuses Phase 30's reserved-name
//! table and UTF-16 length helper rather than restating them.
//!
//! The scan is read-only. Applying the plan renames the source files,
//! which is what makes the subsequent copy land safely — the engine
//! has no per-file destination-rename hook today (Phase 30's
//! `translate_path` is a pure library that the runner never calls), so
//! renaming at the source is the only mechanism that actually changes
//! what a copy writes. Every applied batch is recorded as a `rename`
//! history job, and apply refuses any target that already exists.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::State;

use freally_core::{WINDOWS_MAX_PATH, is_reserved_windows_name, path_utf16_len};
use freally_history::{ItemRow, JobSummary};

use crate::ipc_safety::validate_ipc_path;
use crate::sidecar_commands::rel_display;
use crate::state::AppState;

/// Longest single path component every mainstream filesystem accepts.
const MAX_COMPONENT_LEN: usize = 255;

/// Which destination filesystem's rules to check against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// NTFS / ReFS / exFAT / SMB-to-Windows: the strictest set.
    Windows,
    /// APFS / HFS+: case-insensitive by default, so collisions matter.
    MacOs,
    /// ext4 / xfs / btrfs: only `/` and NUL are illegal, case-sensitive.
    Linux,
}

impl Target {
    /// `pub(crate)` so FFM-M14's preflight report can run the same
    /// rule set the panel shows.
    pub(crate) fn parse(s: &str) -> Result<Self, String> {
        match s {
            "windows" => Ok(Self::Windows),
            "macos" => Ok(Self::MacOs),
            "linux" => Ok(Self::Linux),
            "auto" => Ok(if cfg!(windows) {
                Self::Windows
            } else if cfg!(target_os = "macos") {
                Self::MacOs
            } else {
                Self::Linux
            }),
            other => Err(format!("unknown target filesystem: {other}")),
        }
    }

    /// Whether two names differing only by case would collide.
    fn case_insensitive(self) -> bool {
        matches!(self, Self::Windows | Self::MacOs)
    }
}

/// `true` when `c` cannot appear in a filename on `target`. Path
/// separators are excluded by construction — we only ever inspect
/// single components.
fn is_illegal_char(c: char, target: Target) -> bool {
    if c == '\0' {
        return true;
    }
    match target {
        // The Win32 reserved set, plus every C0 control character.
        Target::Windows => {
            matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') || (c as u32) < 0x20
        }
        // Finder and the POSIX layer disagree about `:` — it is the
        // classic-Mac separator and still shows as `/` in Finder.
        Target::MacOs => c == ':',
        Target::Linux => false,
    }
}

/// Everything wrong with one name. Wire strings are stable; the UI
/// maps them to Fluent keys.
fn issues_for(name: &str, target: Target) -> Vec<&'static str> {
    let mut out = Vec::new();
    if target == Target::Windows && is_reserved_windows_name(name) {
        out.push("reserved-name");
    }
    if name.chars().any(|c| is_illegal_char(c, target)) {
        out.push("illegal-char");
    }
    // Windows silently strips these, so `report.` and `report` become
    // the same file — a data-loss-shaped surprise, not a cosmetic one.
    if target == Target::Windows && name.ends_with(['.', ' ']) {
        out.push("trailing-dot-space");
    }
    if path_utf16_len(Path::new(name)) > MAX_COMPONENT_LEN {
        out.push("component-too-long");
    }
    out
}

/// Rewrite `name` into something `target` accepts. Pure and
/// deterministic; the caller resolves any collisions the rewrite
/// itself introduces.
fn sanitize(name: &str, target: Target) -> String {
    let mut out: String = name
        .chars()
        .map(|c| if is_illegal_char(c, target) { '_' } else { c })
        .collect();

    if target == Target::Windows {
        // Trailing dots/spaces go before the reserved check so
        // `CON. ` is judged on the stem that actually survives.
        while out.ends_with(['.', ' ']) {
            out.pop();
        }
        if is_reserved_windows_name(&out) {
            out = freally_core::apply_reserved_suffix(&out);
        }
    }

    if path_utf16_len(Path::new(&out)) > MAX_COMPONENT_LEN {
        out = truncate_keeping_extension(&out);
    }

    if out.is_empty() {
        out.push('_');
    }
    out
}

/// Shorten an over-long component to [`MAX_COMPONENT_LEN`] while
/// keeping its extension, so a truncated `.tar.gz` is still a `.gz`.
fn truncate_keeping_extension(name: &str) -> String {
    let path = Path::new(name);
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let budget = MAX_COMPONENT_LEN.saturating_sub(ext.chars().count());
    let kept: String = stem.chars().take(budget).collect();
    format!("{kept}{ext}")
}

/// One flagged file.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorRowDto {
    /// Path relative to the scanned root, forward-slashed.
    pub rel: String,
    /// Stable issue wire strings (`"reserved-name"`, `"illegal-char"`,
    /// `"trailing-dot-space"`, `"component-too-long"`,
    /// `"path-too-long"`, `"case-collision"`).
    pub issues: Vec<&'static str>,
    /// The file name the doctor proposes. `None` when renaming the
    /// file alone cannot fix the finding (a path-length overflow
    /// driven by the destination's own depth).
    pub suggested: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorReport {
    /// `"windows"` / `"macos"` / `"linux"` — what the rules were.
    pub target: String,
    /// Files examined.
    pub scanned: u64,
    /// Only the files with findings.
    pub rows: Vec<DoctorRowDto>,
}

/// Build the plan for an already-walked set of relative paths. Split
/// out from the IPC command so the whole rule set is unit-tested
/// without touching a filesystem, and shared with FFM-M14's preflight
/// report so both surfaces flag exactly the same things.
pub(crate) fn build_plan(rels: &[PathBuf], dst_root: &Path, target: Target) -> Vec<DoctorRowDto> {
    // Case-fold collisions are a property of the *set*, not of one
    // name: group the destination-side folded paths and flag every
    // member of a group with more than one distinct original.
    let mut folded: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    if target.case_insensitive() {
        for (i, rel) in rels.iter().enumerate() {
            folded
                .entry(rel_display(rel).to_lowercase())
                .or_default()
                .push(i);
        }
    }
    let collides: std::collections::BTreeSet<usize> = folded
        .values()
        .filter(|group| group.len() > 1)
        .flat_map(|group| group.iter().copied())
        .collect();

    let mut rows = Vec::new();
    for (i, rel) in rels.iter().enumerate() {
        let name = rel
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut issues = issues_for(&name, target);

        // Path length is judged against where the file will *land*,
        // not where it lives now.
        if target == Target::Windows && path_utf16_len(&dst_root.join(rel)) > WINDOWS_MAX_PATH {
            issues.push("path-too-long");
        }
        if collides.contains(&i) {
            issues.push("case-collision");
        }
        if issues.is_empty() {
            continue;
        }

        // A rename can fix everything except a path that is too long
        // because of the destination's own depth — shortening the leaf
        // may not be enough, and the doctor does not invent parent
        // renames.
        let candidate = sanitize(&name, target);
        let name_fixable = issues.iter().any(|i| *i != "path-too-long");
        let suggested = if name_fixable && candidate != name {
            Some(candidate)
        } else {
            None
        };

        rows.push(DoctorRowDto {
            rel: rel_display(rel),
            issues,
            suggested,
        });
    }
    rows
}

/// Scan `src` for names `dst_root`'s filesystem cannot hold. Read-only.
#[tauri::command]
pub async fn filename_doctor_scan(
    src: String,
    dst_root: String,
    target: String,
) -> Result<DoctorReport, String> {
    let src = validate_ipc_path(&src).map_err(|e| e.to_string())?;
    let dst_root = validate_ipc_path(&dst_root).map_err(|e| e.to_string())?;
    let target = Target::parse(&target)?;

    // The walk *and* the judgement both run on the blocking pool:
    // `build_plan` allocates several times per file (fold key, owned
    // name, joined destination path), so on a large tree it would stall
    // every other IPC command if it ran on the async runtime thread.
    let scan_src = src.clone();
    let (scanned, rows) = tauri::async_runtime::spawn_blocking(move || {
        let rels = crate::sidecar_commands::collect_files(&scan_src)?;
        let rows = build_plan(&rels, &dst_root, target);
        Ok::<_, std::io::Error>((rels.len() as u64, rows))
    })
    .await
    .map_err(|e| format!("filename doctor walk failed: {e}"))?
    .map_err(|e| format!("{}: {e}", src.display()))?;

    Ok(DoctorReport {
        target: match target {
            Target::Windows => "windows",
            Target::MacOs => "macos",
            Target::Linux => "linux",
        }
        .to_string(),
        scanned,
        rows,
    })
}

/// What an apply actually did.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorApplyReport {
    pub renamed: u64,
    pub failed: u64,
    /// Per-failure `(rel, reason)`, bounded like every other ledger.
    pub problems: Vec<DoctorProblemDto>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorProblemDto {
    pub rel: String,
    pub reason: String,
}

/// Apply the accepted rows of a plan, renaming each file in place
/// under `root`. Refuses to clobber: a target that already exists is
/// reported as a failure, never overwritten.
#[tauri::command]
pub async fn filename_doctor_apply(
    root: String,
    renames: Vec<(String, String)>,
    state: State<'_, AppState>,
) -> Result<DoctorApplyReport, String> {
    let root = validate_ipc_path(&root).map_err(|e| e.to_string())?;
    let mut report = DoctorApplyReport::default();
    // Every rename that actually happened, so history records what
    // changed rather than just how many did. A destructive batch that
    // cannot be read back afterwards is not auditable.
    let mut applied: Vec<(PathBuf, PathBuf)> = Vec::new();

    for (rel, new_name) in &renames {
        // `rel` must be genuinely *relative*. `Path::join` discards its
        // base when the joined component is absolute or carries a
        // Windows prefix, so `root.join("C:\\Windows\\...")` escapes
        // `root` entirely — and contains no `..`, so the traversal
        // guard alone would wave it through.
        if !is_contained_relative(Path::new(rel)) {
            push_problem(&mut report, rel, format!("not a relative path: {rel}"));
            continue;
        }
        // Still gate it: the traversal / NUL / encoding checks every
        // other path argument gets.
        let from = match validate_ipc_path(&root.join(rel).to_string_lossy()) {
            Ok(p) => p,
            Err(e) => {
                push_problem(&mut report, rel, e.to_string());
                continue;
            }
        };
        // A suggested name is a *file name*, never a path. `C:evil` has
        // no separator and is not `..`, but it carries a drive prefix,
        // so `parent.join` would relocate the rename off `root`.
        if !is_single_component_name(new_name) {
            push_problem(&mut report, rel, format!("invalid new name: {new_name}"));
            continue;
        }
        let Some(parent) = from.parent() else {
            push_problem(&mut report, rel, "path has no parent".to_string());
            continue;
        };
        let to = parent.join(new_name);
        // Belt and braces: whatever the two guards above let through,
        // both ends must still sit under the root the user named.
        if !from.starts_with(&root) || !to.starts_with(&root) {
            push_problem(&mut report, rel, format!("escapes the scanned root: {rel}"));
            continue;
        }
        if to.exists() {
            push_problem(&mut report, rel, format!("already exists: {new_name}"));
            continue;
        }
        match std::fs::rename(&from, &to) {
            Ok(()) => {
                report.renamed += 1;
                applied.push((from, to));
            }
            Err(e) => push_problem(&mut report, rel, e.to_string()),
        }
    }

    record_history(&state, &root, &report, &applied).await;
    Ok(report)
}

/// `true` when `p` is a relative path made only of ordinary
/// components — no root, no Windows prefix, no `..`, no bare `.`.
/// Such a path cannot escape whatever base it is joined onto.
fn is_contained_relative(p: &Path) -> bool {
    use std::path::Component;
    !p.as_os_str().is_empty() && p.components().all(|c| matches!(c, Component::Normal(_)))
}

/// `true` when `name` is exactly one ordinary path component — the
/// contract a "new file name" has to satisfy before it is joined onto
/// a parent directory.
fn is_single_component_name(name: &str) -> bool {
    use std::path::Component;
    let p = Path::new(name);
    let mut it = p.components();
    matches!(it.next(), Some(Component::Normal(_))) && it.next().is_none()
}

fn push_problem(report: &mut DoctorApplyReport, rel: &str, reason: String) {
    const MAX_PROBLEMS: usize = 500;
    report.failed += 1;
    if report.problems.len() < MAX_PROBLEMS {
        report.problems.push(DoctorProblemDto {
            rel: rel.to_string(),
            reason,
        });
    }
}

/// Record the batch as a `rename` history job — one item row per file
/// actually renamed, carrying its `from` → `to` pair, plus one row per
/// failure with its reason. Renaming is destructive, so the record has
/// to be specific enough to reconstruct (or reverse) what happened.
async fn record_history(
    state: &AppState,
    root: &Path,
    report: &DoctorApplyReport,
    applied: &[(PathBuf, PathBuf)],
) {
    let Some(history) = state.history.clone() else {
        return;
    };
    if let Ok(row) = history
        .record_start(&JobSummary {
            kind: "rename".to_string(),
            status: "running".to_string(),
            src_root: root.to_path_buf(),
            dst_root: root.to_path_buf(),
            ..Default::default()
        })
        .await
    {
        for (from, to) in applied {
            let _ = history
                .record_item(&ItemRow {
                    job_row_id: row.0,
                    src: from.clone(),
                    dst: to.clone(),
                    size: 0,
                    status: "ok".to_string(),
                    hash_hex: None,
                    error_code: None,
                    error_msg: None,
                    timestamp_ms: 0,
                })
                .await;
        }
        for problem in &report.problems {
            let _ = history
                .record_item(&ItemRow {
                    job_row_id: row.0,
                    src: root.join(&problem.rel),
                    dst: root.join(&problem.rel),
                    size: 0,
                    status: "failed".to_string(),
                    hash_hex: None,
                    error_code: Some("rename-failed".to_string()),
                    error_msg: Some(problem.reason.clone()),
                    timestamp_ms: 0,
                })
                .await;
        }
        let status = if report.failed == 0 {
            "succeeded"
        } else {
            "failed"
        };
        let _ = history
            .record_finish(row, status, 0, report.renamed, report.failed)
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(names: &[&str], target: Target) -> Vec<DoctorRowDto> {
        let rels: Vec<PathBuf> = names.iter().map(PathBuf::from).collect();
        build_plan(&rels, Path::new(r"D:\dst"), target)
    }

    #[test]
    fn windows_reserved_names_are_flagged_and_suffixed() {
        let rows = plan(&["CON.txt", "fine.txt"], Target::Windows);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].rel, "CON.txt");
        assert!(rows[0].issues.contains(&"reserved-name"));
        assert_eq!(rows[0].suggested.as_deref(), Some("CON_.txt"));
    }

    #[test]
    fn illegal_characters_become_underscores_on_windows_only() {
        let rows = plan(&["a?b*c.txt"], Target::Windows);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].issues.contains(&"illegal-char"));
        assert_eq!(rows[0].suggested.as_deref(), Some("a_b_c.txt"));
        // ext4 accepts every one of those bytes.
        assert!(plan(&["a?b*c.txt"], Target::Linux).is_empty());
    }

    #[test]
    fn macos_flags_a_colon_but_not_the_windows_set() {
        // Driven through the rule functions rather than `build_plan`:
        // `PathBuf::from("a:b.txt")` parses `a:` as a drive prefix on a
        // Windows host, so the name never survives to `file_name()`
        // there. The rule itself is host-independent.
        assert_eq!(issues_for("a:b.txt", Target::MacOs), vec!["illegal-char"]);
        assert_eq!(sanitize("a:b.txt", Target::MacOs), "a_b.txt");
        // The Windows-only set is legal on APFS.
        assert!(issues_for("a?b.txt", Target::MacOs).is_empty());
        assert!(plan(&["a?b.txt"], Target::MacOs).is_empty());
    }

    #[test]
    fn trailing_dots_and_spaces_are_flagged_on_windows() {
        let rows = plan(&["report.", "notes "], Target::Windows);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].issues.contains(&"trailing-dot-space"));
        assert_eq!(rows[0].suggested.as_deref(), Some("report"));
        assert_eq!(rows[1].suggested.as_deref(), Some("notes"));
        // Linux keeps them verbatim.
        assert!(plan(&["report."], Target::Linux).is_empty());
    }

    #[test]
    fn a_reserved_name_hiding_behind_a_trailing_dot_is_still_caught() {
        // `CON. ` → Windows strips the tail, leaving the device name.
        let out = sanitize("CON. ", Target::Windows);
        assert_eq!(out, "CON_");
    }

    #[test]
    fn over_long_components_are_truncated_but_keep_their_extension() {
        let long = format!("{}.txt", "a".repeat(300));
        let rows = plan(&[long.as_str()], Target::Windows);
        assert!(rows[0].issues.contains(&"component-too-long"));
        let suggested = rows[0].suggested.clone().unwrap();
        assert!(suggested.ends_with(".txt"));
        assert_eq!(suggested.chars().count(), MAX_COMPONENT_LEN);
    }

    #[test]
    fn case_fold_collisions_flag_every_member_of_the_group() {
        let rows = plan(&["Readme.md", "README.md", "other.md"], Target::Windows);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.issues.contains(&"case-collision")));
        // Case sensitivity is a destination property — ext4 is fine.
        assert!(plan(&["Readme.md", "README.md"], Target::Linux).is_empty());
    }

    #[test]
    fn a_case_collision_alone_offers_no_rename() {
        // Both names are individually legal; only the pair is a
        // problem, and the doctor will not guess which one to rename.
        let rows = plan(&["Readme.md", "README.md"], Target::Windows);
        assert!(rows.iter().all(|r| r.suggested.is_none()));
    }

    #[test]
    fn path_length_is_judged_against_the_destination() {
        // A short name that lands past MAX_PATH once joined.
        let deep = PathBuf::from("x".repeat(200)).join("y".repeat(80));
        let rows = build_plan(&[deep], Path::new(r"D:\dst"), Target::Windows);
        assert!(rows[0].issues.contains(&"path-too-long"));
    }

    #[test]
    fn a_pure_path_length_overflow_offers_no_rename() {
        let deep = PathBuf::from("x".repeat(200)).join("y".repeat(80));
        let rows = build_plan(&[deep], Path::new(r"D:\dst"), Target::Windows);
        assert_eq!(rows[0].issues, vec!["path-too-long"]);
        assert!(rows[0].suggested.is_none());
    }

    #[test]
    fn clean_trees_produce_no_rows() {
        let rows = plan(&["fine.txt", "also-fine.bin"], Target::Windows);
        assert!(rows.is_empty());
    }

    #[test]
    fn target_parse_covers_every_wire_name() {
        assert_eq!(Target::parse("windows").unwrap(), Target::Windows);
        assert_eq!(Target::parse("macos").unwrap(), Target::MacOs);
        assert_eq!(Target::parse("linux").unwrap(), Target::Linux);
        assert!(Target::parse("auto").is_ok());
        assert!(Target::parse("bsd").is_err());
    }

    #[test]
    fn containment_guard_rejects_anything_that_escapes_a_join() {
        // Ordinary relative paths are the only accepted shape.
        assert!(is_contained_relative(Path::new("a/b/c.txt")));
        assert!(is_contained_relative(Path::new("file.txt")));
        // Absolute / rooted / prefixed paths silently replace the base
        // they are joined onto, so they must never get through.
        assert!(!is_contained_relative(Path::new("/etc/hosts")));
        assert!(!is_contained_relative(Path::new("../escape")));
        assert!(!is_contained_relative(Path::new("")));
        #[cfg(windows)]
        {
            assert!(!is_contained_relative(Path::new(r"C:\Windows\System32")));
            assert!(!is_contained_relative(Path::new(r"\\server\share\f")));
        }
    }

    #[test]
    fn a_new_name_must_be_exactly_one_component() {
        assert!(is_single_component_name("safe.txt"));
        assert!(!is_single_component_name(""));
        assert!(!is_single_component_name(".."));
        assert!(!is_single_component_name("a/b"));
        assert!(!is_single_component_name("/abs"));
        // `C:evil` carries a drive prefix but no separator — the old
        // separator-only check let it through and `parent.join` then
        // relocated the rename off the scanned root.
        #[cfg(windows)]
        assert!(!is_single_component_name("C:evil"));
    }

    #[test]
    fn sanitize_never_returns_an_empty_name() {
        assert_eq!(sanitize("...", Target::Windows), "_");
        assert_eq!(sanitize("   ", Target::Windows), "_");
    }
}
