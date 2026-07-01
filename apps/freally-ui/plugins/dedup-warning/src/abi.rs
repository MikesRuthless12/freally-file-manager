//! ABI helpers shared by every Phase 46.5 sample plugin. See
//! `apps/freally-ui/plugins/organize-by-exif/src/abi.rs` for the
//! authoritative reference docstring.

use serde::Serialize;

pub fn alloc(size: i32) -> i32 {
    if size <= 0 {
        return 0;
    }
    let buf: Box<[u8]> = vec![0u8; size as usize].into_boxed_slice();
    Box::leak(buf).as_ptr() as u32 as i32
}

pub fn with_ctx<F, T>(ctx_ptr: i32, ctx_len: i32, f: F) -> T
where
    F: FnOnce(&[u8]) -> T,
{
    let bytes: &[u8] = if ctx_len <= 0 {
        &[]
    } else {
        // SAFETY: the host writes exactly `ctx_len` bytes at
        // `ctx_ptr` before invoking `hook`.
        unsafe { std::slice::from_raw_parts(ctx_ptr as u32 as *const u8, ctx_len as usize) }
    };
    f(bytes)
}

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
