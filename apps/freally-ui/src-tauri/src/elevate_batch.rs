//! FFM-M16 — whole-job elevated batch with a consent ledger.
//!
//! Phase 8's `retry_elevated` fixes **one** file and asks for consent
//! each time; a job that hit two hundred permission-denied paths turns
//! that into two hundred UAC dialogs, which is how people learn to
//! click "yes" without reading.
//!
//! This is the batch form:
//!
//! 1. [`elevate_batch_ledger`] enumerates exactly which paths the
//!    helper would touch — every item of a history job whose failure
//!    was a permission denial, source *and* destination — so the user
//!    approves a concrete list, not a vague "run as administrator".
//! 2. The UI shows that ledger for approval.
//! 3. [`elevate_batch_apply`] retries each pair unprivileged first (a
//!    denial that has since been fixed must not cost an escalation),
//!    then escalates whatever is left through **one** spawn of the
//!    existing DACL-restricted, single-capability helper.
//! 4. The approved ledger is appended to the audit log: one
//!    `ElevationGranted` record carrying the job and the exact path
//!    count, then the usual per-file records, so an auditor can
//!    reconcile the consent against what it actually authorised.
//!
//! Nothing here widens the helper's capability set — it is the same
//! `Capability::ElevatedRetry` grant the single-file path uses.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::State;

use freally_helper::rpc::Response;
use freally_history::JobRowId;

use crate::ipc_safety::validate_ipc_path;
use crate::state::AppState;

/// Engine error classes that a privilege escalation can actually fix.
/// Anything else in the failed ledger (a full disk, a missing source)
/// stays out of the batch — escalating would not help and the consent
/// would be for nothing.
const ELEVATABLE: &[&str] = &["permission-denied", "access-denied"];

/// One row of the consent ledger: a path pair the helper will touch.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerEntryDto {
    pub src: String,
    pub dst: String,
    /// The engine error class that put this row in the batch.
    pub error_code: String,
}

/// The ledger the user is asked to approve.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsentLedgerDto {
    pub job_id: i64,
    pub entries: Vec<LedgerEntryDto>,
    /// Failed items excluded because elevation cannot fix them.
    pub not_elevatable: u64,
}

/// Enumerate the permission-denied paths of a job. Read-only — this is
/// what the consent dialog renders before anything is escalated.
#[tauri::command]
pub async fn elevate_batch_ledger(
    job_id: i64,
    state: State<'_, AppState>,
) -> Result<ConsentLedgerDto, String> {
    let history = state.history.clone().ok_or("history is disabled")?;
    let items = history
        .items_for(JobRowId(job_id))
        .await
        .map_err(|e| e.to_string())?;

    let mut entries = Vec::new();
    let mut not_elevatable = 0u64;
    for item in items.into_iter().filter(|i| i.status == "failed") {
        let code = item.error_code.unwrap_or_default();
        if ELEVATABLE.contains(&code.as_str()) {
            entries.push(LedgerEntryDto {
                src: item.src.to_string_lossy().into_owned(),
                dst: item.dst.to_string_lossy().into_owned(),
                error_code: code,
            });
        } else {
            not_elevatable += 1;
        }
    }

    Ok(ConsentLedgerDto {
        job_id,
        entries,
        not_elevatable,
    })
}

/// Per-entry outcome of an applied batch.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchRowDto {
    pub src: String,
    pub dst: String,
    /// `"ok"` (copied) or `"failed"`.
    pub status: String,
    pub bytes: u64,
    /// Localized error key when `status == "failed"`.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchApplyReport {
    pub copied: u64,
    pub failed: u64,
    pub bytes: u64,
    /// `true` when at least one entry needed the elevated helper — i.e.
    /// a consent prompt was actually shown.
    pub elevated: bool,
    pub rows: Vec<BatchRowDto>,
}

/// Map a helper response into a ledger row outcome.
fn row_from_response(src: String, dst: String, resp: &Response) -> BatchRowDto {
    match resp {
        Response::ElevatedRetryOk { bytes } => BatchRowDto {
            src,
            dst,
            status: "ok".to_string(),
            bytes: *bytes,
            error: None,
        },
        Response::ElevatedRetryFailed { localized_key, .. }
        | Response::PathRejected { localized_key, .. } => BatchRowDto {
            src,
            dst,
            status: "failed".to_string(),
            bytes: 0,
            error: Some(localized_key.clone()),
        },
        Response::CapabilityDenied { .. } => BatchRowDto {
            src,
            dst,
            status: "failed".to_string(),
            bytes: 0,
            error: Some("retry-elevated-unavailable".to_string()),
        },
        other => BatchRowDto {
            src,
            dst,
            status: "failed".to_string(),
            bytes: 0,
            error: Some(format!("err-helper-unexpected:{other:?}")),
        },
    }
}

/// Roll the per-row outcomes into the report totals.
fn summarize(rows: Vec<BatchRowDto>, elevated: bool) -> BatchApplyReport {
    let mut report = BatchApplyReport {
        elevated,
        ..Default::default()
    };
    for row in &rows {
        if row.status == "ok" {
            report.copied += 1;
            report.bytes += row.bytes;
        } else {
            report.failed += 1;
        }
    }
    report.rows = rows;
    report
}

/// Apply an approved ledger. Every entry is retried unprivileged
/// first; only what still needs privilege reaches the single elevated
/// spawn, so a batch whose permissions were fixed in the meantime asks
/// for no consent at all.
///
/// **The caller does not get to name the paths.** `entries` is a
/// *selection* over the ledger this job actually produced, not the
/// source of truth: the authoritative set is re-derived here from the
/// history DB and anything submitted that is not in it aborts the whole
/// call. Without that, a forged WebView script — the threat
/// `ipc_safety` is explicitly written against — could hand arbitrary
/// `src`/`dst` pairs to a helper running as Administrator/root, or swap
/// the list between the consent dialog and this command so the user
/// approves one set of paths and a different set executes.
/// `commands::retry_elevated` resolves its single pair server-side from
/// the error registry for exactly this reason; the batch form keeps
/// that property.
#[tauri::command]
pub async fn elevate_batch_apply(
    job_id: i64,
    entries: Vec<LedgerEntryDto>,
    state: State<'_, AppState>,
) -> Result<BatchApplyReport, String> {
    use freally_helper::capability::Capability;
    use freally_helper::rpc::Request;
    use freally_helper::{handle_request, should_escalate};

    if entries.is_empty() {
        return Ok(BatchApplyReport::default());
    }

    // Re-derive what this job is actually allowed to escalate. The
    // submitted list may be a *subset* (the user deselected rows) but
    // never a superset: any row not in the derived ledger means the
    // list was tampered with between consent and execution, so the
    // whole batch is refused rather than silently escalating the rest.
    let submitted = entries.len();
    let authoritative = elevate_batch_ledger(job_id, state.clone()).await?;
    let allowed: std::collections::BTreeSet<(String, String)> = authoritative
        .entries
        .into_iter()
        .map(|e| (e.src, e.dst))
        .collect();
    let entries: Vec<LedgerEntryDto> = entries
        .into_iter()
        .filter(|e| allowed.contains(&(e.src.clone(), e.dst.clone())))
        .collect();
    if entries.len() != submitted {
        return Err("elevate-batch-ledger-mismatch".to_string());
    }

    // Every path crosses the IPC boundary, so it gets the same lexical
    // gate the single-file path applies — before any escalation.
    let mut pairs: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(entries.len());
    for entry in &entries {
        let src = validate_ipc_path(&entry.src).map_err(|e| e.to_string())?;
        let dst = validate_ipc_path(&entry.dst).map_err(|e| e.to_string())?;
        pairs.push((src, dst));
    }

    // Tier 1 — unprivileged, on the blocking pool (the same
    // `handle_request` body `retry_elevated` uses).
    let tier1_pairs = pairs.clone();
    let tier1 = tauri::async_runtime::spawn_blocking(move || {
        let granted = vec![Capability::ElevatedRetry];
        tier1_pairs
            .into_iter()
            .map(|(src, dst)| handle_request(&Request::ElevatedRetry { src, dst }, &granted))
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| format!("err-helper-join:{e}"))?;

    // Tier 2 — one spawn, one consent, for everything still denied.
    let needs_elevation: Vec<usize> = tier1
        .iter()
        .enumerate()
        .filter(|(_, resp)| should_escalate(resp))
        .map(|(i, _)| i)
        .collect();

    let mut responses = tier1;
    let elevated = !needs_elevation.is_empty();
    if elevated {
        // The consent covers exactly the paths the ledger listed, so it
        // is recorded *before* the prompt — an auditor sees what was
        // asked for even if the user then cancels.
        crate::audit_commands::record_elevation_granted(
            &state.audit,
            &format!("job-{job_id}"),
            needs_elevation.len() as u64,
        );
        let batch: Vec<(PathBuf, PathBuf)> =
            needs_elevation.iter().map(|i| pairs[*i].clone()).collect();
        match crate::elevate::elevated_retry_batch(&batch).await {
            Ok(elevated_responses) => {
                for (slot, resp) in needs_elevation.iter().zip(elevated_responses) {
                    responses[*slot] = resp;
                }
            }
            Err(_) => {
                // A cancelled or unavailable consent leaves the
                // unprivileged denials in place — each row reports the
                // same key the single-file path uses.
                for slot in &needs_elevation {
                    responses[*slot] = Response::CapabilityDenied {
                        reason: "elevation unavailable or consent declined".to_string(),
                    };
                }
            }
        }
    }

    let rows: Vec<BatchRowDto> = entries
        .into_iter()
        .zip(responses.iter())
        .map(|(entry, resp)| row_from_response(entry.src, entry.dst, resp))
        .collect();

    // Per-file audit records, so the consent above is reconcilable
    // against what it actually authorised.
    let job_tag = format!("job-{job_id}");
    for row in &rows {
        if row.status == "ok" {
            crate::audit_commands::record_file_copied(
                &state.audit,
                &job_tag,
                std::path::Path::new(&row.src),
                std::path::Path::new(&row.dst),
                None,
                row.bytes,
            );
        } else {
            crate::audit_commands::record_file_failed(
                &state.audit,
                &job_tag,
                std::path::Path::new(&row.src),
                row.error.as_deref().unwrap_or("unknown"),
                row.error.as_deref().unwrap_or(""),
            );
        }
    }

    Ok(summarize(rows, elevated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_permission_denials_are_elevatable() {
        assert!(ELEVATABLE.contains(&"permission-denied"));
        // A full disk is not a privilege problem — escalating would
        // spend a consent prompt on something it cannot fix.
        assert!(!ELEVATABLE.contains(&"no-space"));
        assert!(!ELEVATABLE.contains(&"not-found"));
    }

    #[test]
    fn an_ok_response_becomes_a_copied_row() {
        let row = row_from_response(
            "/a".to_string(),
            "/b".to_string(),
            &Response::ElevatedRetryOk { bytes: 4096 },
        );
        assert_eq!(row.status, "ok");
        assert_eq!(row.bytes, 4096);
        assert!(row.error.is_none());
    }

    #[test]
    fn a_rejected_path_reports_its_localized_key() {
        let row = row_from_response(
            "/a".to_string(),
            "/b".to_string(),
            &Response::PathRejected {
                localized_key: "err-path-escape".to_string(),
                offending: PathBuf::from(".."),
            },
        );
        assert_eq!(row.status, "failed");
        assert_eq!(row.error.as_deref(), Some("err-path-escape"));
    }

    #[test]
    fn a_denied_capability_maps_to_the_shared_unavailable_key() {
        let row = row_from_response(
            "/a".to_string(),
            "/b".to_string(),
            &Response::CapabilityDenied {
                reason: "not granted".to_string(),
            },
        );
        assert_eq!(row.error.as_deref(), Some("retry-elevated-unavailable"));
    }

    #[test]
    fn summarize_counts_bytes_only_for_copied_rows() {
        let rows = vec![
            BatchRowDto {
                src: "/a".into(),
                dst: "/b".into(),
                status: "ok".into(),
                bytes: 100,
                error: None,
            },
            BatchRowDto {
                src: "/c".into(),
                dst: "/d".into(),
                status: "failed".into(),
                bytes: 0,
                error: Some("err-permission-denied".into()),
            },
        ];
        let report = summarize(rows, true);
        assert_eq!(report.copied, 1);
        assert_eq!(report.failed, 1);
        assert_eq!(report.bytes, 100);
        assert!(report.elevated);
        assert_eq!(report.rows.len(), 2);
    }
}
