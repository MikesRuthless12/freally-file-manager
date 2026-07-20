//! FFM-M07 — failed-file ledger + retry-failed pass.
//!
//! The Phase 9 history DB already records every failed item (path +
//! error class + message). This module surfaces that ledger to the UI
//! and exports it (TXT / CSV / JSON) so the failed set can be reviewed,
//! retried (the frontend re-enqueues each failed source through the
//! normal verified copy path), or fed into a later `--files-from`
//! import (FFM-M13).

use serde::Serialize;
use tauri::State;

use freally_history::JobRowId;

use crate::state::AppState;

/// One row of the failed-file ledger.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedItemDto {
    pub src: String,
    pub dst: String,
    /// Kebab-case engine error class (`"permission-denied"`, …), if any.
    pub error_code: Option<String>,
    pub error_msg: Option<String>,
}

/// Every failed item of a history job, newest job's failures first.
#[tauri::command]
pub async fn job_failed_items(
    job_id: i64,
    state: State<'_, AppState>,
) -> Result<Vec<FailedItemDto>, String> {
    let history = state.history.clone().ok_or("history is disabled")?;
    let items = history
        .items_for(JobRowId(job_id))
        .await
        .map_err(|e| e.to_string())?;
    Ok(items
        .into_iter()
        .filter(|i| i.status == "failed")
        .map(|i| FailedItemDto {
            src: i.src.to_string_lossy().into_owned(),
            dst: i.dst.to_string_lossy().into_owned(),
            error_code: i.error_code,
            error_msg: i.error_msg,
        })
        .collect())
}

/// Render the failed set in `format` (`"txt" | "csv" | "json"`). Pure
/// so the shape is unit-tested without a live history DB.
fn render_failed(items: &[FailedItemDto], format: &str) -> Result<String, String> {
    match format {
        // One source path per line — directly consumable as an
        // FFM-M13 `--files-from` manifest.
        "txt" => Ok(items
            .iter()
            .map(|i| i.src.clone())
            .collect::<Vec<_>>()
            .join("\n")),
        "csv" => {
            let mut out = String::from("src,dst,error_code,error_msg\n");
            for i in items {
                out.push_str(&format!(
                    "{},{},{},{}\n",
                    csv_field(&i.src),
                    csv_field(&i.dst),
                    csv_field(i.error_code.as_deref().unwrap_or("")),
                    csv_field(i.error_msg.as_deref().unwrap_or("")),
                ));
            }
            Ok(out)
        }
        "json" => serde_json::to_string_pretty(items).map_err(|e| e.to_string()),
        other => Err(format!("unknown export format: {other}")),
    }
}

/// RFC-4180 CSV field: quote when it contains a comma, quote, or
/// newline; double any embedded quote.
fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Export the failed set of `job_id` to `dest` in the given format.
#[tauri::command]
pub async fn export_failed_items(
    job_id: i64,
    format: String,
    dest: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let items = job_failed_items(job_id, state).await?;
    let body = render_failed(&items, &format)?;
    let path = crate::ipc_safety::validate_ipc_path(&dest).map_err(|e| e.to_string())?;
    tokio::fs::write(&path, body)
        .await
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<FailedItemDto> {
        vec![
            FailedItemDto {
                src: r"C:\a\file, one.txt".to_string(),
                dst: r"D:\b\file, one.txt".to_string(),
                error_code: Some("permission-denied".to_string()),
                error_msg: Some("Access is denied".to_string()),
            },
            FailedItemDto {
                src: "/plain/two.bin".to_string(),
                dst: "/dst/two.bin".to_string(),
                error_code: None,
                error_msg: None,
            },
        ]
    }

    #[test]
    fn txt_is_one_source_per_line() {
        let out = render_failed(&sample(), "txt").unwrap();
        assert_eq!(out, "C:\\a\\file, one.txt\n/plain/two.bin");
    }

    #[test]
    fn csv_quotes_fields_with_commas() {
        let out = render_failed(&sample(), "csv").unwrap();
        assert!(out.starts_with("src,dst,error_code,error_msg\n"));
        // The comma-bearing path is quoted; the plain one is not.
        assert!(out.contains("\"C:\\a\\file, one.txt\""));
        assert!(out.contains("/plain/two.bin,/dst/two.bin,,\n"));
    }

    #[test]
    fn json_round_trips() {
        let out = render_failed(&sample(), "json").unwrap();
        assert!(out.contains("\"permission-denied\""));
        assert!(out.contains("\"src\""));
    }

    #[test]
    fn unknown_format_errors() {
        assert!(render_failed(&sample(), "xml").is_err());
    }
}
