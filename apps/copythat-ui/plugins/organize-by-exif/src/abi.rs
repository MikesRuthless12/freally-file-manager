//! ABI helpers shared by every Phase 46.5 sample plugin.
//!
//! Hand-rolled here (rather than pulled from a crate) so a
//! third-party plugin author can copy a single source file as a
//! starting point. Keeps the `crates/copythat-plugin` host crate
//! free of a wasm-only sub-crate.
//!
//! Memory model: each `alloc` leaks a `Box<[u8]>` into the Rust
//! global allocator. The host builds a fresh `wasmtime::Store` per
//! `call_hook` dispatch, so the leaked buffers are reclaimed when
//! the store drops at the end of the call. Within a single call the
//! plugin may allocate twice (once for the host-supplied `HookCtx`
//! buffer in `alloc`, once for the response buffer in
//! `pack_outcome`); both buffers stay live until the store is torn
//! down.

use serde::Serialize;

/// Allocate `size` bytes of plugin-side linear memory and return a
/// pointer to the start of the buffer. The host writes the
/// JSON-encoded `HookCtx` into this region before calling `hook`.
///
/// Returns `0` for `size <= 0` so a zero-length payload from the
/// host (which the host's encoder never produces, but a malicious
/// host could) cannot allocate a zero-sized boxed slice (UB).
pub fn alloc(size: i32) -> i32 {
    if size <= 0 {
        return 0;
    }
    let buf: Box<[u8]> = vec![0u8; size as usize].into_boxed_slice();
    Box::leak(buf).as_ptr() as u32 as i32
}

/// Borrow the host-supplied `HookCtx` bytes, hand them to `f`, and
/// return whatever outcome `f` produces. Centralises the unsafe
/// `from_raw_parts` call so per-plugin `lib.rs` files stay
/// `unsafe`-clean apart from the FFI declarations themselves.
///
/// `ctx_len <= 0` is treated as "empty payload"; `f` receives a
/// zero-length slice in that case.
pub fn with_ctx<F, T>(ctx_ptr: i32, ctx_len: i32, f: F) -> T
where
    F: FnOnce(&[u8]) -> T,
{
    let bytes: &[u8] = if ctx_len <= 0 {
        &[]
    } else {
        // SAFETY: the host's `PluginHandle::call_hook` writes
        // exactly `ctx_len` bytes starting at `ctx_ptr` into the
        // plugin's linear memory before invoking `hook`. The pointer
        // and length therefore identify a valid, initialised slice
        // for the lifetime of this call.
        unsafe { std::slice::from_raw_parts(ctx_ptr as u32 as *const u8, ctx_len as usize) }
    };
    f(bytes)
}

/// Serialise `outcome` to JSON, leak the bytes into linear memory,
/// and return the packed `(out_ptr << 32) | out_len` pair the host's
/// `hook` ABI expects. Falls back to a hard-coded `{"kind":"continue"}`
/// payload if serialisation somehow fails so the host never sees a
/// truncated JSON response.
pub fn pack_outcome<T: Serialize>(outcome: &T) -> i64 {
    let bytes = match serde_json::to_vec(outcome) {
        Ok(v) => v,
        Err(_) => CONTINUE_FALLBACK.as_bytes().to_vec(),
    };
    pack_bytes(bytes.into_boxed_slice())
}

const CONTINUE_FALLBACK: &str = r#"{"kind":"continue"}"#;

fn pack_bytes(buf: Box<[u8]>) -> i64 {
    let leaked: &'static [u8] = Box::leak(buf);
    let ptr = leaked.as_ptr() as u32 as i64;
    let len = leaked.len() as i64;
    (ptr << 32) | len
}
