//! Constants mirroring `freally-core`'s engine bounds.
//!
//! The settings crate intentionally doesn't depend on `freally-core`
//! so it stays a lean preference + IO layer. Instead we mirror the
//! buffer-size window here. If the engine ever relaxes its bounds,
//! update both in one commit — the `freally-core` change + this
//! constant + any `effective_buffer_size` callers.

/// Mirror of `freally_core::options::DEFAULT_BUFFER_SIZE` (1 MiB).
pub const DEFAULT_BUFFER_SIZE: usize = 1024 * 1024;

/// Mirror of `freally_core::options::MIN_BUFFER_SIZE` (64 KiB).
pub const MIN_BUFFER_SIZE: usize = 64 * 1024;

/// Mirror of `freally_core::options::MAX_BUFFER_SIZE` (16 MiB).
pub const MAX_BUFFER_SIZE: usize = 16 * 1024 * 1024;

/// Default system-wide hotkey for "paste files via Freally File Manager".
/// Tauri's `global-shortcut` plugin resolves `CmdOrCtrl` to Cmd on
/// macOS and Ctrl on Windows / Linux, so one spelling covers all three
/// hosts. `Shift` avoids colliding with the platform-native paste
/// (Cmd+V / Ctrl+V) so users keep normal text-paste semantics.
pub const DEFAULT_PASTE_SHORTCUT: &str = "CmdOrCtrl+Shift+V";

/// Phase 42 — default retry count for sharing-violation opens.
/// Mirrors `freally_core::CopyOptions::sharing_violation_retries`
/// (the engine's pre-Phase-42 hardcoded short-loop count). Exposed as
/// a `fn` so it can be used as `#[serde(default = "...")]` on
/// `TransferSettings::sharing_violation_retries`.
#[inline]
pub const fn default_sharing_violation_retries() -> u32 {
    3
}

/// Phase 42 — default base delay (ms) between sharing-violation
/// retries. Mirrors
/// `freally_core::CopyOptions::sharing_violation_base_delay_ms`.
/// Exposed as a `fn` so it can be used as `#[serde(default = "...")]`
/// on `TransferSettings::sharing_violation_base_delay_ms`.
#[inline]
pub const fn default_sharing_violation_base_delay_ms() -> u64 {
    50
}
