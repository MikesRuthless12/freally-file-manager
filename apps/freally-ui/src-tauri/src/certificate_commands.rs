//! FFM-M10 — copy-verification certificate export.
//!
//! The Phase 43 provenance manifest is a machine-verifiable CBOR
//! artifact that needs the CLI to check. This is the other half: the
//! document you hand a client or attach to a ticket — one completed
//! job rendered as printable HTML (browser → PDF), or as CSV / JSON
//! for a spreadsheet or a pipeline.
//!
//! Everything in a certificate comes from the Phase 9 history DB, so
//! it can be produced long after the job ran: job metadata + timings,
//! every per-file row with its verify digest and size, and the error
//! ledger. (Per-file collision *decisions* are not separately
//! persisted; a collision the engine resolved by skipping shows up as
//! that file's `skipped` status, which is what the ledger reports.)
//!
//! Signing is optional and reuses the Phase 43 ed25519 key: point the
//! export at a PKCS#8 PEM private key and it writes a detached
//! `<report>.sig` next to the report, carrying the public key so a
//! recipient can verify without any Freally-specific tooling.

use std::path::Path;

use serde::Serialize;
use tauri::State;

use freally_history::{ItemRow, JobRowId, JobSummary};

use crate::failed_commands::csv_field;
use crate::ipc_safety::validate_ipc_path;
use crate::report_util::{html_escape, now_ms, with_suffix};
use crate::state::AppState;

/// One per-file row of a certificate.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CertificateFileDto {
    pub src: String,
    pub dst: String,
    pub size: u64,
    /// `"ok"` / `"failed"` / `"skipped"` / `"cancelled"`.
    pub status: String,
    /// Verify digest, when verification ran for this file.
    pub hash_hex: Option<String>,
    pub error_code: Option<String>,
    pub error_msg: Option<String>,
    pub timestamp_ms: i64,
}

/// The whole certificate, before rendering.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CertificateDto {
    pub job_id: i64,
    pub kind: String,
    pub status: String,
    pub src_root: String,
    pub dst_root: String,
    pub started_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    /// `None` while the job is still running.
    pub duration_ms: Option<i64>,
    pub total_bytes: u64,
    pub files_ok: u64,
    pub files_failed: u64,
    /// `None` when the job ran with verification off.
    pub verify_algo: Option<String>,
    /// The app build that produced the certificate.
    pub app_version: String,
    pub generated_at_ms: i64,
    pub files: Vec<CertificateFileDto>,
}

/// Assemble a certificate from the history rows. Pure so the shape is
/// unit-tested without a live DB.
fn build_certificate(
    job: &JobSummary,
    items: Vec<ItemRow>,
    generated_at_ms: i64,
) -> CertificateDto {
    CertificateDto {
        job_id: job.row_id,
        kind: job.kind.clone(),
        status: job.status.clone(),
        src_root: job.src_root.to_string_lossy().into_owned(),
        dst_root: job.dst_root.to_string_lossy().into_owned(),
        started_at_ms: job.started_at_ms,
        finished_at_ms: job.finished_at_ms,
        duration_ms: job.finished_at_ms.map(|f| f - job.started_at_ms),
        total_bytes: job.total_bytes,
        files_ok: job.files_ok,
        files_failed: job.files_failed,
        verify_algo: job.verify_algo.clone(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        generated_at_ms,
        files: items
            .into_iter()
            .map(|i| CertificateFileDto {
                src: i.src.to_string_lossy().into_owned(),
                dst: i.dst.to_string_lossy().into_owned(),
                size: i.size,
                status: i.status,
                hash_hex: i.hash_hex,
                error_code: i.error_code,
                error_msg: i.error_msg,
                timestamp_ms: i.timestamp_ms,
            })
            .collect(),
    }
}

/// Render a certificate in `format` (`"html" | "csv" | "json"`).
fn render_certificate(cert: &CertificateDto, format: &str) -> Result<String, String> {
    match format {
        "json" => serde_json::to_string_pretty(cert).map_err(|e| e.to_string()),
        "csv" => Ok(render_csv(cert)),
        "html" => Ok(render_html(cert)),
        other => Err(format!("unknown certificate format: {other}")),
    }
}

fn render_csv(cert: &CertificateDto) -> String {
    let mut out = String::from("src,dst,size,status,hash,error_code,error_msg,timestamp_ms\n");
    for f in &cert.files {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{}\n",
            csv_field(&f.src),
            csv_field(&f.dst),
            f.size,
            csv_field(&f.status),
            csv_field(f.hash_hex.as_deref().unwrap_or("")),
            csv_field(f.error_code.as_deref().unwrap_or("")),
            csv_field(f.error_msg.as_deref().unwrap_or("")),
            f.timestamp_ms,
        ));
    }
    out
}

/// A self-contained printable document — no external CSS, no scripts,
/// so "print to PDF" in any browser produces the same page the user
/// saw.
fn render_html(cert: &CertificateDto) -> String {
    let mut rows = String::new();
    for f in &cert.files {
        rows.push_str(&format!(
            "<tr class=\"s-{}\"><td>{}</td><td>{}</td><td class=\"n\">{}</td><td>{}</td><td class=\"h\">{}</td><td>{}</td></tr>\n",
            html_escape(&f.status),
            html_escape(&f.src),
            html_escape(&f.dst),
            f.size,
            html_escape(&f.status),
            html_escape(f.hash_hex.as_deref().unwrap_or("—")),
            html_escape(
                f.error_msg
                    .as_deref()
                    .or(f.error_code.as_deref())
                    .unwrap_or("")
            ),
        ));
    }

    let verify = cert.verify_algo.as_deref().unwrap_or("off");
    let duration = cert
        .duration_ms
        .map(|d| d.to_string())
        .unwrap_or_else(|| "—".to_string());
    let finished = cert
        .finished_at_ms
        .map(|f| f.to_string())
        .unwrap_or_else(|| "—".to_string());

    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<title>Freally copy-verification certificate — job {job_id}</title>
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
td.n {{ text-align: right; font-variant-numeric: tabular-nums; }}
td.h {{ font-family: ui-monospace, Consolas, monospace; word-break: break-all; }}
tr.s-failed {{ background: #fdecec; }}
tr.s-skipped {{ background: #fdf6e6; }}
@media print {{ body {{ margin: 0; }} th {{ background: none; }} }}
</style></head><body>
<h1>Copy-verification certificate</h1>
<p class="sub">Freally File Manager {app_version} · generated {generated_at_ms} (ms since epoch, UTC)</p>
<dl>
<dt>Job</dt><dd>#{job_id} — {kind} — {status}</dd>
<dt>Source</dt><dd>{src_root}</dd>
<dt>Destination</dt><dd>{dst_root}</dd>
<dt>Started</dt><dd>{started_at_ms}</dd>
<dt>Finished</dt><dd>{finished}</dd>
<dt>Duration (ms)</dt><dd>{duration}</dd>
<dt>Bytes</dt><dd>{total_bytes}</dd>
<dt>Files OK / failed</dt><dd>{files_ok} / {files_failed}</dd>
<dt>Verification</dt><dd>{verify}</dd>
</dl>
<table>
<thead><tr><th>Source</th><th>Destination</th><th>Size</th><th>Status</th><th>Digest</th><th>Error</th></tr></thead>
<tbody>
{rows}</tbody>
</table>
</body></html>
"#,
        job_id = cert.job_id,
        kind = html_escape(&cert.kind),
        status = html_escape(&cert.status),
        src_root = html_escape(&cert.src_root),
        dst_root = html_escape(&cert.dst_root),
        started_at_ms = cert.started_at_ms,
        finished = finished,
        duration = duration,
        total_bytes = cert.total_bytes,
        files_ok = cert.files_ok,
        files_failed = cert.files_failed,
        verify = html_escape(verify),
        app_version = html_escape(&cert.app_version),
        generated_at_ms = cert.generated_at_ms,
        rows = rows,
    )
}

/// The detached signature written beside a signed certificate. Plain
/// JSON so a recipient can verify with any ed25519 tool: the public
/// key is SPKI PEM, the signature is over the certificate file's exact
/// bytes.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DetachedSignature {
    algorithm: &'static str,
    /// Basename of the file these bytes were signed over.
    file: String,
    public_key_pem: String,
    signature_hex: String,
}

/// Sign `body` with the PKCS#8 PEM key at `key_path` and return the
/// detached-signature document.
fn sign_body(key_path: &Path, file_name: &str, body: &[u8]) -> Result<String, String> {
    let pem =
        std::fs::read_to_string(key_path).map_err(|e| format!("{}: {e}", key_path.display()))?;
    let key = freally_provenance::signing_key_from_pem(&pem).map_err(|e| e.to_string())?;
    let doc = DetachedSignature {
        algorithm: "ed25519",
        file: file_name.to_string(),
        public_key_pem: freally_provenance::verifying_key_to_pem(&key.verifying_key())
            .map_err(|e| e.to_string())?,
        signature_hex: hex::encode(freally_provenance::sign_detached(&key, body)),
    };
    serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())
}

/// What the export wrote, for the completion toast.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CertificateExportReport {
    pub path: String,
    /// Present when a signing key was supplied.
    pub signature_path: Option<String>,
    pub files: u64,
}

/// Export a completed job as a certificate. `format` is
/// `"html" | "csv" | "json"`; `signing_key` is an optional path to a
/// PKCS#8 PEM ed25519 private key (`freally provenance keygen`).
#[tauri::command]
pub async fn export_job_certificate(
    job_id: i64,
    format: String,
    dest: String,
    signing_key: Option<String>,
    state: State<'_, AppState>,
) -> Result<CertificateExportReport, String> {
    let dest = validate_ipc_path(&dest).map_err(|e| e.to_string())?;
    let key_path = match signing_key.as_deref() {
        Some(p) => Some(validate_ipc_path(p).map_err(|e| e.to_string())?),
        None => None,
    };

    let history = state.history.clone().ok_or("history is disabled")?;
    let job = history
        .get(JobRowId(job_id))
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no history job with id {job_id}"))?;
    let items = history
        .items_for(JobRowId(job_id))
        .await
        .map_err(|e| e.to_string())?;

    let cert = build_certificate(&job, items, now_ms());
    let body = render_certificate(&cert, &format)?;
    tokio::fs::write(&dest, &body)
        .await
        .map_err(|e| format!("{}: {e}", dest.display()))?;

    let signature_path = match key_path {
        Some(key) => {
            let name = dest
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let doc = sign_body(&key, &name, body.as_bytes())?;
            let sig_path = with_suffix(&dest, ".sig");
            tokio::fs::write(&sig_path, doc)
                .await
                .map_err(|e| format!("{}: {e}", sig_path.display()))?;
            Some(sig_path.to_string_lossy().into_owned())
        }
        None => None,
    };

    Ok(CertificateExportReport {
        path: dest.to_string_lossy().into_owned(),
        signature_path,
        files: cert.files.len() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample_job() -> JobSummary {
        JobSummary {
            row_id: 7,
            kind: "copy".to_string(),
            status: "succeeded".to_string(),
            started_at_ms: 1_000,
            finished_at_ms: Some(3_500),
            src_root: PathBuf::from(r"C:\src"),
            dst_root: PathBuf::from(r"D:\dst"),
            total_bytes: 2048,
            files_ok: 1,
            files_failed: 1,
            verify_algo: Some("blake3".to_string()),
            options_json: None,
        }
    }

    fn sample_items() -> Vec<ItemRow> {
        vec![
            ItemRow {
                job_row_id: 7,
                src: PathBuf::from(r"C:\src\a & b.txt"),
                dst: PathBuf::from(r"D:\dst\a & b.txt"),
                size: 1024,
                status: "ok".to_string(),
                hash_hex: Some("abc123".to_string()),
                error_code: None,
                error_msg: None,
                timestamp_ms: 2_000,
            },
            ItemRow {
                job_row_id: 7,
                src: PathBuf::from("/src/two, comma.bin"),
                dst: PathBuf::from("/dst/two, comma.bin"),
                size: 0,
                status: "failed".to_string(),
                hash_hex: None,
                error_code: Some("permission-denied".to_string()),
                error_msg: Some("Access is denied".to_string()),
                timestamp_ms: 3_000,
            },
        ]
    }

    fn sample_cert() -> CertificateDto {
        build_certificate(&sample_job(), sample_items(), 9_999)
    }

    #[test]
    fn certificate_carries_job_metadata_and_duration() {
        let cert = sample_cert();
        assert_eq!(cert.job_id, 7);
        assert_eq!(cert.duration_ms, Some(2_500));
        assert_eq!(cert.verify_algo.as_deref(), Some("blake3"));
        assert_eq!(cert.files.len(), 2);
        assert_eq!(cert.app_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(cert.generated_at_ms, 9_999);
    }

    #[test]
    fn running_job_has_no_duration() {
        let mut job = sample_job();
        job.finished_at_ms = None;
        job.status = "running".to_string();
        let cert = build_certificate(&job, Vec::new(), 1);
        assert_eq!(cert.duration_ms, None);
    }

    #[test]
    fn html_escapes_paths_and_keeps_the_document_self_contained() {
        let html = render_certificate(&sample_cert(), "html").unwrap();
        // The ampersand in the path must not reach the document raw.
        assert!(html.contains("a &amp; b.txt"), "{html}");
        assert!(!html.contains("a & b.txt"));
        // No external resources — printing offline must look the same.
        assert!(!html.contains("<script"));
        assert!(!html.contains("http://") && !html.contains("https://"));
        // The error ledger is part of the document.
        assert!(html.contains("Access is denied"));
        assert!(html.contains("blake3"));
    }

    #[test]
    fn html_escapes_an_angle_bracket_path() {
        let mut job = sample_job();
        job.src_root = PathBuf::from("/src/<script>evil</script>");
        let html = render_certificate(&build_certificate(&job, Vec::new(), 0), "html").unwrap();
        assert!(!html.contains("<script>evil"));
        assert!(html.contains("&lt;script&gt;evil"));
    }

    #[test]
    fn csv_has_a_header_and_quotes_comma_paths() {
        let csv = render_certificate(&sample_cert(), "csv").unwrap();
        assert!(csv.starts_with("src,dst,size,status,hash,error_code,error_msg,timestamp_ms\n"));
        assert!(csv.contains("\"/src/two, comma.bin\""));
        assert!(csv.contains(",1024,ok,abc123,,,2000\n"));
    }

    #[test]
    fn json_round_trips_the_dto() {
        let json = render_certificate(&sample_cert(), "json").unwrap();
        assert!(json.contains("\"jobId\": 7"));
        assert!(json.contains("\"verifyAlgo\": \"blake3\""));
        assert!(json.contains("\"permission-denied\""));
    }

    #[test]
    fn unknown_format_errors() {
        assert!(render_certificate(&sample_cert(), "pdf").is_err());
    }

    #[test]
    fn signed_body_verifies_against_the_embedded_public_key() {
        let dir = tempfile::tempdir().unwrap();
        let key = freally_provenance::generate_signing_key();
        let key_path = dir.path().join("signing.pem");
        std::fs::write(
            &key_path,
            freally_provenance::signing_key_to_pem(&key).unwrap(),
        )
        .unwrap();

        let body = b"certificate bytes";
        let doc = sign_body(&key_path, "report.html", body).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&doc).unwrap();
        assert_eq!(parsed["algorithm"], "ed25519");
        assert_eq!(parsed["file"], "report.html");

        let pem = parsed["publicKeyPem"].as_str().unwrap();
        let verifying = freally_provenance::verifying_key_from_pem(pem).unwrap();
        let sig_hex = parsed["signatureHex"].as_str().unwrap();
        let raw = hex::decode(sig_hex).unwrap();
        freally_provenance::verify_detached(&verifying, body, &raw).unwrap();
        // A tampered report must not verify against the same signature.
        assert!(freally_provenance::verify_detached(&verifying, b"tampered bytes", &raw).is_err());
    }
}
