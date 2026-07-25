//! FFM-M14 — preflight & dry-run report export.
//!
//! The CLI's `plan --json` already emits machine events, but there is
//! no way to keep the plan: this writes the dry-run out as CSV, JSON,
//! or a printable HTML document *before* the job runs, so a change
//! window can be reviewed and signed off on paper.
//!
//! Everything in the report comes from surfaces that already exist —
//! Phase 41's tree diff for the per-category rows and byte totals, and
//! FFM-M12's filename doctor for the destination-legality findings — so
//! the exported artifact and the on-screen gates can never disagree.
//! Nothing here writes to the source or the destination; the only file
//! produced is the report itself, and the run is recorded as a
//! `preflight` history row so a plan is attachable to history before a
//! byte moves.

use std::path::Path;

use serde::Serialize;
use tauri::State;

use freally_history::{ItemRow, JobSummary};

use crate::failed_commands::csv_field;
use crate::filename_doctor::{DoctorRowDto, Target};
use crate::ipc_safety::validate_ipc_path;
use crate::preview_commands::{DryRunOptionsDto, TreeDiffDto, compute_tree_diff};
use crate::report_util::{html_escape, now_ms};
use crate::state::AppState;

/// The whole plan, before rendering.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightReportDto {
    pub src: String,
    pub dst: String,
    pub app_version: String,
    pub generated_at_ms: i64,
    pub bytes_to_transfer: u64,
    pub bytes_total: u64,
    pub total_files: usize,
    pub additions: Vec<String>,
    pub replacements: Vec<CategoryRowDto>,
    pub skips: Vec<CategoryRowDto>,
    pub conflicts: Vec<CategoryRowDto>,
    pub unchanged: Vec<String>,
    /// FFM-M12 findings for the same source/destination pair.
    pub filename_findings: Vec<DoctorRowDto>,
}

/// A per-category row: the path plus why it landed in that category.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryRowDto {
    pub rel_path: String,
    pub detail: String,
}

fn build_report(
    src: &str,
    dst: &str,
    diff: TreeDiffDto,
    filename_findings: Vec<DoctorRowDto>,
    generated_at_ms: i64,
) -> PreflightReportDto {
    PreflightReportDto {
        src: src.to_string(),
        dst: dst.to_string(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        generated_at_ms,
        bytes_to_transfer: diff.bytes_to_transfer,
        bytes_total: diff.bytes_total,
        total_files: diff.total_files,
        additions: diff.additions,
        replacements: diff
            .replacements
            .into_iter()
            .map(|r| CategoryRowDto {
                rel_path: r.rel_path,
                detail: r.reason.to_string(),
            })
            .collect(),
        skips: diff
            .skips
            .into_iter()
            .map(|s| CategoryRowDto {
                rel_path: s.rel_path,
                detail: s.reason.to_string(),
            })
            .collect(),
        conflicts: diff
            .conflicts
            .into_iter()
            .map(|c| CategoryRowDto {
                rel_path: c.rel_path,
                detail: c.kind.to_string(),
            })
            .collect(),
        unchanged: diff.unchanged,
        filename_findings,
    }
}

/// Flatten every category into `(category, path, detail)` triples —
/// the shape both the CSV and the HTML table render from, so the two
/// can never drift apart.
fn flat_rows(report: &PreflightReportDto) -> Vec<(&'static str, String, String)> {
    let mut rows: Vec<(&'static str, String, String)> = Vec::new();
    for p in &report.additions {
        rows.push(("add", p.clone(), String::new()));
    }
    for r in &report.replacements {
        rows.push(("replace", r.rel_path.clone(), r.detail.clone()));
    }
    for r in &report.skips {
        rows.push(("skip", r.rel_path.clone(), r.detail.clone()));
    }
    for r in &report.conflicts {
        rows.push(("conflict", r.rel_path.clone(), r.detail.clone()));
    }
    for p in &report.unchanged {
        rows.push(("unchanged", p.clone(), String::new()));
    }
    for f in &report.filename_findings {
        rows.push(("filename", f.rel.clone(), f.issues.join(" ")));
    }
    rows
}

fn render_csv(report: &PreflightReportDto) -> String {
    let mut out = String::from("category,path,detail\n");
    for (category, path, detail) in flat_rows(report) {
        out.push_str(&format!(
            "{},{},{}\n",
            category,
            csv_field(&path),
            csv_field(&detail)
        ));
    }
    out
}

fn render_html(report: &PreflightReportDto) -> String {
    let mut rows = String::new();
    for (category, path, detail) in flat_rows(report) {
        rows.push_str(&format!(
            "<tr class=\"c-{}\"><td>{}</td><td>{}</td><td>{}</td></tr>\n",
            category,
            category,
            html_escape(&path),
            html_escape(&detail),
        ));
    }
    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<title>Freally preflight report</title>
<style>
body {{ font-family: system-ui, sans-serif; font-size: 13px; margin: 24px; color: #1f1f1f; }}
h1 {{ font-size: 18px; margin: 0 0 4px; }}
.sub {{ color: #666; margin: 0 0 18px; }}
dl {{ display: grid; grid-template-columns: max-content 1fr; gap: 4px 16px; margin: 0 0 20px; }}
dt {{ color: #666; }}
dd {{ margin: 0; word-break: break-all; }}
table {{ border-collapse: collapse; width: 100%; font-size: 11px; }}
th, td {{ border-bottom: 1px solid #ddd; padding: 4px 6px; text-align: left; vertical-align: top; }}
th {{ background: #f4f4f4; }}
tr.c-conflict {{ background: #fdecec; }}
tr.c-filename {{ background: #fdf6e6; }}
@media print {{ body {{ margin: 0; }} th {{ background: none; }} }}
</style></head><body>
<h1>Preflight report</h1>
<p class="sub">Freally File Manager {app_version} · generated {generated_at_ms} (ms since epoch, UTC) · nothing has been written</p>
<dl>
<dt>Source</dt><dd>{src}</dd>
<dt>Destination</dt><dd>{dst}</dd>
<dt>Files in plan</dt><dd>{total_files}</dd>
<dt>Bytes to transfer</dt><dd>{bytes_to_transfer} of {bytes_total}</dd>
<dt>Add / replace / skip</dt><dd>{adds} / {replaces} / {skips}</dd>
<dt>Conflicts</dt><dd>{conflicts}</dd>
<dt>Filename findings</dt><dd>{findings}</dd>
</dl>
<table>
<thead><tr><th>Category</th><th>Path</th><th>Detail</th></tr></thead>
<tbody>
{rows}</tbody>
</table>
</body></html>
"#,
        app_version = html_escape(&report.app_version),
        generated_at_ms = report.generated_at_ms,
        src = html_escape(&report.src),
        dst = html_escape(&report.dst),
        total_files = report.total_files,
        bytes_to_transfer = report.bytes_to_transfer,
        bytes_total = report.bytes_total,
        adds = report.additions.len(),
        replaces = report.replacements.len(),
        skips = report.skips.len(),
        conflicts = report.conflicts.len(),
        findings = report.filename_findings.len(),
        rows = rows,
    )
}

fn render_report(report: &PreflightReportDto, format: &str) -> Result<String, String> {
    match format {
        "json" => serde_json::to_string_pretty(report).map_err(|e| e.to_string()),
        "csv" => Ok(render_csv(report)),
        "html" => Ok(render_html(report)),
        other => Err(format!("unknown preflight report format: {other}")),
    }
}

/// What the export wrote.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightExportReport {
    pub path: String,
    pub total_files: usize,
    pub filename_findings: usize,
}

/// Compute the plan for `src` → `dst` and write it to `dest` as
/// `"html" | "csv" | "json"`. Reads only; the report file is the sole
/// thing created.
#[tauri::command]
pub async fn export_preflight_report(
    src: String,
    dst: String,
    opts: DryRunOptionsDto,
    target: String,
    format: String,
    dest: String,
    state: State<'_, AppState>,
) -> Result<PreflightExportReport, String> {
    let src_path = validate_ipc_path(&src).map_err(|e| e.to_string())?;
    let dst_path = validate_ipc_path(&dst).map_err(|e| e.to_string())?;
    let dest_path = validate_ipc_path(&dest).map_err(|e| e.to_string())?;
    let target = Target::parse(&target)?;

    let diff = compute_tree_diff(src.clone(), dst.clone(), opts).await?;

    // Same walk + rule set the FFM-M12 panel runs, so the paper report
    // and the on-screen gate always agree.
    let scan_src = src_path.clone();
    let scan_dst = dst_path.clone();
    let filename_findings = tauri::async_runtime::spawn_blocking(move || {
        let rels = crate::sidecar_commands::collect_files(&scan_src).unwrap_or_default();
        crate::filename_doctor::build_plan(&rels, &scan_dst, target)
    })
    .await
    .map_err(|e| format!("preflight filename scan failed: {e}"))?;

    let report = build_report(&src, &dst, diff, filename_findings, now_ms());
    let body = render_report(&report, &format)?;
    tokio::fs::write(&dest_path, body)
        .await
        .map_err(|e| format!("{}: {e}", dest_path.display()))?;

    record_history(&state, &src_path, &dst_path, &report).await;
    Ok(PreflightExportReport {
        path: dest_path.to_string_lossy().into_owned(),
        total_files: report.total_files,
        filename_findings: report.filename_findings.len(),
    })
}

async fn record_history(state: &AppState, src: &Path, dst: &Path, report: &PreflightReportDto) {
    let Some(history) = state.history.clone() else {
        return;
    };
    if let Ok(row) = history
        .record_start(&JobSummary {
            kind: "preflight".to_string(),
            status: "running".to_string(),
            src_root: src.to_path_buf(),
            dst_root: dst.to_path_buf(),
            total_bytes: report.bytes_to_transfer,
            ..Default::default()
        })
        .await
    {
        let _ = history
            .record_item(&ItemRow {
                job_row_id: row.0,
                src: src.to_path_buf(),
                dst: dst.to_path_buf(),
                size: 0,
                status: "ok".to_string(),
                hash_hex: None,
                error_code: None,
                error_msg: None,
                timestamp_ms: 0,
            })
            .await;
        let _ = history
            .record_finish(
                row,
                "succeeded",
                report.bytes_to_transfer,
                report.total_files as u64,
                0,
            )
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview_commands::{ConflictRowDto, ReplacementRowDto, SkipRowDto};

    fn sample_diff() -> TreeDiffDto {
        TreeDiffDto {
            additions: vec!["new/a & b.txt".to_string()],
            replacements: vec![ReplacementRowDto {
                rel_path: "over/written.bin".to_string(),
                reason: "source-newer",
            }],
            skips: vec![SkipRowDto {
                rel_path: "same, comma.txt".to_string(),
                reason: "identical-content",
            }],
            conflicts: vec![ConflictRowDto {
                rel_path: "clash.dat".to_string(),
                kind: "kind-mismatch",
            }],
            unchanged: vec!["untouched.md".to_string()],
            bytes_to_transfer: 1024,
            bytes_total: 4096,
            total_files: 5,
            has_blocking_conflicts: true,
        }
    }

    fn sample_findings() -> Vec<DoctorRowDto> {
        vec![DoctorRowDto {
            rel: "CON.txt".to_string(),
            issues: vec!["reserved-name"],
            suggested: Some("CON_.txt".to_string()),
        }]
    }

    fn sample_report() -> PreflightReportDto {
        build_report("/src", "/dst", sample_diff(), sample_findings(), 7_777)
    }

    #[test]
    fn every_category_reaches_the_flat_rows() {
        let rows = flat_rows(&sample_report());
        let categories: Vec<&str> = rows.iter().map(|(c, _, _)| *c).collect();
        for expected in [
            "add",
            "replace",
            "skip",
            "conflict",
            "unchanged",
            "filename",
        ] {
            assert!(categories.contains(&expected), "missing {expected}");
        }
        assert_eq!(rows.len(), 6);
    }

    #[test]
    fn csv_has_a_header_and_quotes_comma_paths() {
        let csv = render_csv(&sample_report());
        assert!(csv.starts_with("category,path,detail\n"));
        assert!(csv.contains("\"same, comma.txt\""));
        assert!(csv.contains("replace,over/written.bin,source-newer\n"));
        assert!(csv.contains("filename,CON.txt,reserved-name\n"));
    }

    #[test]
    fn html_escapes_paths_and_stays_self_contained() {
        let html = render_html(&sample_report());
        assert!(html.contains("new/a &amp; b.txt"));
        assert!(!html.contains("a & b.txt"));
        assert!(!html.contains("<script"));
        assert!(!html.contains("http://") && !html.contains("https://"));
        // The report must state plainly that it changed nothing.
        assert!(html.contains("nothing has been written"));
    }

    #[test]
    fn json_carries_the_totals_and_the_doctor_findings() {
        let json = render_report(&sample_report(), "json").unwrap();
        assert!(json.contains("\"bytesToTransfer\": 1024"));
        assert!(json.contains("\"totalFiles\": 5"));
        assert!(json.contains("\"reserved-name\""));
        assert!(json.contains("\"generatedAtMs\": 7777"));
    }

    #[test]
    fn unknown_format_errors() {
        assert!(render_report(&sample_report(), "pdf").is_err());
    }
}
