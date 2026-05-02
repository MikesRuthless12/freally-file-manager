//! `organize-by-exif` — Phase 46.5 sample plugin.
//!
//! Registers `HookKind::AfterFile`. After the engine finishes
//! copying a single file, this plugin inspects the EXIF `DateTaken`
//! metadata the host supplies in `HookCtx::data` and proposes a
//! reorganised destination path under `<dest_root>/YYYY/MM/`. The
//! proposal is returned as `HookOutcome::Notify { message }`; the
//! engine then either honours the move automatically or surfaces it
//! to the user, depending on the user's per-plugin trust setting.
//!
//! # ABI contract
//!
//! Mirrors the host's `crates/copythat-plugin/src/handle.rs` ABI
//! constants:
//! * Linear memory exported as `memory` (cdylib gives this for free).
//! * `alloc(size: i32) -> i32` — return a pointer to a fresh
//!   `size`-byte buffer the host writes into.
//! * `hook(ctx_ptr: i32, ctx_len: i32) -> i64` — read the
//!   JSON-encoded `HookCtx` at `[ctx_ptr, ctx_ptr + ctx_len)`,
//!   compute a `HookOutcome`, write its JSON form to a fresh buffer,
//!   and return the packed `(out_ptr << 32) | out_len`.
//!
//! # Capability declaration
//!
//! The shipped `plugin.toml` requests `read_fs:source` and
//! `write_fs:dest`. In the 46.5 runtime the EXIF data is supplied
//! pre-extracted in `HookCtx::data`, so the plugin does not actually
//! call into a WASI fd_read import yet — but the manifest shape is
//! forward-compatible with the WASI wiring that lands alongside the
//! Settings UI in 46.6/46.7, where the plugin will do its own
//! filesystem access via preopened directory handles.
//!
//! # Expected `HookCtx::data` schema (JSON, host-supplied)
//!
//! ```json
//! {
//!   "src_path": "/source/IMG_0001.jpg",
//!   "dest_path": "/dest/IMG_0001.jpg",
//!   "dest_root": "/dest",
//!   "exif": { "date_taken_iso": "2024-03-15T10:30:00Z" }
//! }
//! ```
//!
//! Behaviour:
//! * `data.exif.date_taken_iso` is missing or unparseable → return
//!   `HookOutcome::Continue` (no proposal).
//! * `data.dest_root` or `data.dest_path` is missing → return
//!   `HookOutcome::Continue` (cannot construct a target path).
//! * Otherwise → return `HookOutcome::Notify { message }` carrying a
//!   one-line `move <src> -> <dest_root>/YYYY/MM/<basename>`
//!   proposal the engine surfaces to the user.

use serde::{Deserialize, Serialize};

mod abi;

#[derive(Deserialize)]
struct HookCtx {
    #[serde(default)]
    data: CtxData,
}

#[derive(Deserialize, Default)]
struct CtxData {
    #[serde(default)]
    src_path: Option<String>,
    #[serde(default)]
    dest_path: Option<String>,
    #[serde(default)]
    dest_root: Option<String>,
    #[serde(default)]
    exif: Option<Exif>,
}

#[derive(Deserialize)]
struct Exif {
    /// `YYYY-MM-DDTHH:MM:SS[Z|±HH:MM]` per ISO 8601. The plugin only
    /// reads the leading `YYYY-MM` portion; the rest is ignored.
    #[serde(default)]
    date_taken_iso: Option<String>,
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
    let exif_iso = ctx
        .data
        .exif
        .as_ref()
        .and_then(|e| e.date_taken_iso.as_deref());
    let Some(iso) = exif_iso else {
        return HookOutcome::Continue;
    };
    let Some((year, month)) = parse_year_month(iso) else {
        return HookOutcome::Continue;
    };
    let (Some(dest_root), Some(dest_path)) = (
        ctx.data.dest_root.as_deref(),
        ctx.data.dest_path.as_deref(),
    ) else {
        return HookOutcome::Continue;
    };
    let Some(basename) = path_basename(dest_path) else {
        return HookOutcome::Continue;
    };
    let src = ctx.data.src_path.as_deref().unwrap_or(dest_path);
    let proposal = format!(
        "organize-by-exif: move {src} -> {dest_root}/{year:04}/{month:02}/{basename}"
    );
    HookOutcome::Notify { message: proposal }
}

/// Parse the `YYYY-MM` prefix of an ISO 8601 date-time. Anything
/// past the first 7 characters is ignored; `2024-03` → `(2024, 3)`,
/// `2024/03/15` → `None`. Returns `None` for any non-conforming
/// input so the dispatcher's fallback (`HookOutcome::Continue`)
/// kicks in cleanly.
fn parse_year_month(iso: &str) -> Option<(u16, u8)> {
    let bytes = iso.as_bytes();
    if bytes.len() < 7 {
        return None;
    }
    if bytes[4] != b'-' {
        return None;
    }
    let year_str = std::str::from_utf8(&bytes[0..4]).ok()?;
    let month_str = std::str::from_utf8(&bytes[5..7]).ok()?;
    let year: u16 = year_str.parse().ok()?;
    let month: u8 = month_str.parse().ok()?;
    if !(1..=12).contains(&month) {
        return None;
    }
    Some((year, month))
}

/// Lift the trailing path component out of an absolute or relative
/// path. Splits on both `/` and `\` so Windows + POSIX dest paths
/// from the host both flow through cleanly.
fn path_basename(path: &str) -> Option<&str> {
    let last = path
        .rsplit_once(|c| c == '/' || c == '\\')
        .map(|(_, tail)| tail)
        .unwrap_or(path);
    if last.is_empty() { None } else { Some(last) }
}
