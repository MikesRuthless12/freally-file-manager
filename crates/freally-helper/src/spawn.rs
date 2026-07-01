//! Phase 17d — caller-side privilege-escalation spawner.
//!
//! Orchestrates launching the `freally-helper` binary ELEVATED
//! (Windows UAC `Start-Process -Verb RunAs` / Linux `pkexec` / macOS
//! `osascript … with administrator privileges`) and driving the
//! JSON-RPC handshake over a per-launch random named pipe / socket.
//!
//! This crate stays `#![forbid(unsafe_code)]`: the actual elevated
//! exec uses `std::process::Command` (safe), and the secure
//! named-pipe SERVER (Windows DACL FFI) is delegated to
//! `freally-platform` — mirroring how `freally-snapshot`'s VSS
//! helper keeps its unsafe corner out of the forbid-clean crates.
//!
//! Build order (see docs/ROADMAP.md Phase 17d): this module first
//! ships the two PURE, host-stable decision pieces every spawn path
//! is built on — [`should_escalate`] (when a permission failure
//! warrants a consent prompt) and [`build_spawn_command`] (the exact
//! OS-native exec, constructed without spawning so it is unit-
//! testable). The live pipe server + spawn + handshake land on top.

use crate::capability::Capability;
use crate::rpc::Response;

/// Decide whether a failed in-process `ElevatedRetry` warrants
/// escalating to the elevated helper.
///
/// Only a genuine permission / access-denied failure is worth a UAC /
/// polkit / osascript consent prompt. A tainted path
/// (`err-path-escape`), a missing source (`err-not-found`), or any
/// other I/O error cannot be fixed by elevation, so we never prompt
/// the user for those — we surface the original error instead.
pub fn should_escalate(resp: &Response) -> bool {
    matches!(
        resp,
        Response::ElevatedRetryFailed { localized_key, .. }
            if localized_key.as_str() == "err-permission-denied"
    )
}

/// Build the OS-native command that launches `helper_path` elevated,
/// telling it which pipe / socket to connect back on and which
/// capabilities the caller requests (the argv UPPER bound of the
/// Phase 17j two-phase grant — the matching lower bound is sent over
/// the pipe via `Request::GrantCapabilities`).
///
/// Pure: returns `(program, args)` for a `std::process::Command`
/// WITHOUT spawning, so the consent-flow argv can be asserted in
/// tests where the real UAC / polkit dialog cannot be driven.
/// `pipe_or_socket` MUST come from [`crate::rpc::generate_pipe_name`].
pub fn build_spawn_command(
    helper_path: &str,
    pipe_or_socket: &str,
    capabilities: &[Capability],
) -> (String, Vec<String>) {
    let caps = capabilities
        .iter()
        .map(|c| c.wire_label())
        .collect::<Vec<_>>()
        .join(",");

    #[cfg(target_os = "windows")]
    {
        // UAC: relaunch via PowerShell `Start-Process -Verb RunAs`.
        // Start-Process severs std-handle inheritance, so the elevated
        // child connects back over the named pipe rather than stdio.
        // Embedded single-quotes are doubled (PowerShell escaping); the
        // caller resolves an absolute powershell.exe path to defend
        // against PATH hijacking.
        let inner = format!(
            "Start-Process -Verb RunAs -WindowStyle Hidden -FilePath '{}' \
             -ArgumentList @('--pipe={}','--capabilities={}')",
            ps_escape(helper_path),
            ps_escape(pipe_or_socket),
            ps_escape(&caps),
        );
        (
            "powershell.exe".to_string(),
            vec![
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
                inner,
            ],
        )
    }
    #[cfg(target_os = "linux")]
    {
        // GUI consent via polkit; headless callers fall back to `sudo`.
        (
            "pkexec".to_string(),
            vec![
                helper_path.to_string(),
                format!("--socket={pipe_or_socket}"),
                format!("--capabilities={caps}"),
            ],
        )
    }
    #[cfg(target_os = "macos")]
    {
        // System auth dialog; the child runs as root.
        let script = format!(
            "do shell script \"{} --socket={} --capabilities={}\" \
             with administrator privileges",
            sh_escape(helper_path),
            sh_escape(pipe_or_socket),
            sh_escape(&caps),
        );
        ("osascript".to_string(), vec!["-e".to_string(), script])
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        let _ = (helper_path, pipe_or_socket, caps);
        ("true".to_string(), Vec::new())
    }
}

/// PowerShell single-quote escaping: a literal `'` is doubled inside a
/// single-quoted string.
#[cfg(target_os = "windows")]
fn ps_escape(s: &str) -> String {
    s.replace('\'', "''")
}

/// AppleScript string escaping for the `do shell script "…"` literal.
#[cfg(target_os = "macos")]
fn sh_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_escalate_only_on_permission_denied() {
        // The one case worth a consent prompt:
        assert!(should_escalate(&Response::ElevatedRetryFailed {
            localized_key: "err-permission-denied".into(),
            message: "access denied".into(),
        }));
        // Everything else must NOT escalate:
        assert!(!should_escalate(&Response::ElevatedRetryOk { bytes: 10 }));
        assert!(!should_escalate(&Response::ElevatedRetryFailed {
            localized_key: "err-not-found".into(),
            message: "missing source".into(),
        }));
        assert!(!should_escalate(&Response::PathRejected {
            offending: "..".into(),
            localized_key: "err-path-escape".into(),
        }));
        assert!(!should_escalate(&Response::CapabilityDenied {
            reason: "not granted".into(),
        }));
    }

    #[test]
    fn build_spawn_command_carries_pipe_and_capability() {
        let (program, args) = build_spawn_command(
            "/opt/freally/freally-helper",
            "freally-helper-pipe-0123456789abcdef",
            &[Capability::ElevatedRetry],
        );
        let joined = args.join(" ");
        assert!(!program.is_empty(), "program must be set");
        assert!(
            joined.contains("freally-helper-pipe-0123456789abcdef"),
            "the pipe/socket name must be forwarded: {joined}"
        );
        assert!(
            joined.contains("elevated_retry"),
            "the requested capability must be forwarded: {joined}"
        );
        assert!(
            joined.contains("freally-helper") || program.contains("freally-helper"),
            "the helper path must be referenced: program={program} args={joined}"
        );
    }

    #[test]
    fn build_spawn_command_joins_multiple_capabilities() {
        let (_program, args) = build_spawn_command(
            "/opt/freally/freally-helper",
            "freally-helper-pipe-0123456789abcdef",
            &[Capability::ElevatedRetry, Capability::ShellExtension],
        );
        let joined = args.join(" ");
        assert!(
            joined.contains("elevated_retry,shell_extension"),
            "{joined}"
        );
    }
}
