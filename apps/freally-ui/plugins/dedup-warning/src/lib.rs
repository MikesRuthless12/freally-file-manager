//! `dedup-warning` — Phase 46.5 sample plugin.
//!
//! Registers `HookKind::AfterJob`. Reads the duplicate-file count
//! the host accumulated during the job (deduplication is performed
//! by the dedup pass in the engine, surfaced to plugins via
//! `HookCtx::data.duplicates_found`) and, if it exceeds the
//! configured threshold, returns a `HookOutcome::Notify` so the
//! engine surfaces a tray/log notification.
//!
//! # Capability declaration
//!
//! Empty — this plugin reads only data already in the host-supplied
//! `HookCtx` and emits an outcome the host already knows how to
//! handle. No filesystem or network access required.
//!
//! # Expected `HookCtx::data` schema (JSON, host-supplied)
//!
//! ```json
//! {
//!   "job_id": "5f3c…",
//!   "duplicates_found": 47,
//!   "files_total": 1234,
//!   "threshold": 10
//! }
//! ```
//!
//! `threshold` defaults to `10` when absent so a freshly-installed
//! plugin still produces meaningful warnings before the user has had
//! a chance to tune the value via the Settings UI (lands in 46.6).
//!
//! # Outcome
//!
//! * `duplicates_found <= threshold` → `HookOutcome::Continue`.
//! * `duplicates_found > threshold` → `HookOutcome::Notify { message }`
//!   carrying a one-line summary suitable for the tray.

use serde::{Deserialize, Serialize};

mod abi;

const DEFAULT_THRESHOLD: u64 = 10;

#[derive(Deserialize)]
struct HookCtx {
    #[serde(default)]
    data: CtxData,
}

#[derive(Deserialize, Default)]
struct CtxData {
    #[serde(default)]
    job_id: Option<String>,
    #[serde(default)]
    duplicates_found: Option<u64>,
    #[serde(default)]
    files_total: Option<u64>,
    #[serde(default)]
    threshold: Option<u64>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum HookOutcome {
    Continue,
    Notify { message: String },
}

#[unsafe(no_mangle)]
pub extern "C" fn alloc(size: i32) -> i32 {
    abi::alloc(size)
}

#[unsafe(no_mangle)]
pub extern "C" fn hook(ctx_ptr: i32, ctx_len: i32) -> i64 {
    let outcome = abi::with_ctx(ctx_ptr, ctx_len, compute);
    abi::pack_outcome(&outcome)
}

fn compute(ctx_bytes: &[u8]) -> HookOutcome {
    let ctx: HookCtx = match serde_json::from_slice(ctx_bytes) {
        Ok(c) => c,
        Err(_) => return HookOutcome::Continue,
    };
    let dups = ctx.data.duplicates_found.unwrap_or(0);
    let threshold = ctx.data.threshold.unwrap_or(DEFAULT_THRESHOLD);
    if dups <= threshold {
        return HookOutcome::Continue;
    }
    let total = ctx.data.files_total.unwrap_or(0);
    let job = ctx.data.job_id.as_deref().unwrap_or("(unset)");
    let pct = if total > 0 {
        format!(" ({:.1}% of {})", (dups as f64 / total as f64) * 100.0, total)
    } else {
        String::new()
    };
    let message = format!(
        "dedup-warning: job {job} processed {dups} duplicates{pct} (threshold {threshold})"
    );
    HookOutcome::Notify { message }
}
