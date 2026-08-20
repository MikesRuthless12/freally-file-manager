//! Phase 17d — caller-side privilege-escalation spawner.
//!
//! Orchestrates launching the `freally-helper` binary ELEVATED
//! (Windows UAC `Start-Process -Verb RunAs` / Linux `pkexec` / macOS
//! `osascript … with administrator privileges`) and driving the
//! JSON-RPC handshake over a per-launch rendezvous the parent owns.
//!
//! # Why Unix rendezvouses on a loopback port, not a socket file
//!
//! The Unix side used to bind a `UnixListener` at a random path and
//! hand that path to the elevated child. A filesystem rendezvous is
//! not defensible against an attacker running as the **same uid**:
//! they can `unlink` our node and `bind` their own while the consent
//! dialog is up. The user then authenticates, the root helper dials
//! the attacker, and the attacker drives it — `elevated_retry` with
//! `dst: /etc/sudoers.d/…` is a root write, `src: /etc/shadow` a root
//! read. An unguessable name does not help (the child is given it on
//! an argv anyone can read), and neither does a shared secret, for
//! the same reason.
//!
//! A listening TCP socket on `127.0.0.1` has no filesystem node to
//! unlink and cannot be taken over: the parent binds port 0, holds
//! the listener across the whole consent window, and never sets
//! `SO_REUSEPORT`, so a second `bind` of that port fails. There is no
//! window in which the child can reach anyone but us.
//!
//! What this does not stop: a same-uid attacker can still *connect*
//! to that port and answer as if it were the helper. That costs them
//! nothing and gains them nothing — they are the client, so they can
//! spoof a result but cannot ask for a privileged operation. The
//! parent keeps accepting until a peer completes the handshake, so a
//! connection that stays silent or speaks the wrong protocol cannot
//! consume the one accept the real helper needs. Distinguishing the
//! two properly needs `SO_PEERCRED` / `LOCAL_PEERPID`, which means
//! `libc` and `unsafe` in two crates that forbid it; the escalation
//! path is closed either way, which is the part that mattered.
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
/// telling it where to connect back and which capabilities the caller
/// requests (the argv UPPER bound of the Phase 17j two-phase grant —
/// the matching lower bound is sent over the connection via
/// `Request::GrantCapabilities`).
///
/// `endpoint` is platform-shaped, and the caller must supply the
/// matching form:
///
/// - **Windows** — the named pipe, from
///   [`crate::rpc::generate_pipe_name`]. The pipe server is created
///   before the spawn with `FILE_FLAG_FIRST_PIPE_INSTANCE` and a
///   two-ACE DACL, so it cannot be pre-empted or unlinked.
/// - **Unix** — the decimal port of a `127.0.0.1` listener the parent
///   already holds. See the module docs for why this is not a socket
///   path.
///
/// Pure: returns `(program, args)` for a `std::process::Command`
/// WITHOUT spawning, so the consent-flow argv can be asserted in
/// tests where the real UAC / polkit dialog cannot be driven.
pub fn build_spawn_command(
    helper_path: &str,
    endpoint: &str,
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
            ps_escape(endpoint),
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
        // `--port=`, not `--socket=`: the rendezvous is a loopback
        // listener the parent holds, so there is no node for a
        // same-uid process to unlink out from under us.
        (
            "pkexec".to_string(),
            vec![
                helper_path.to_string(),
                format!("--port={endpoint}"),
                format!("--capabilities={caps}"),
            ],
        )
    }
    #[cfg(target_os = "macos")]
    {
        // System auth dialog; the child runs as root.
        // `--port=`, not `--socket=` — see the module docs. This
        // matters more here than on Linux: `do shell script` cannot
        // inherit a file descriptor, so a socketpair is not available
        // as an alternative and the loopback listener is the only way
        // to hold the rendezvous ourselves.
        let script = format!(
            "do shell script \"{} --port={} --capabilities={}\" \
             with administrator privileges",
            sh_escape(helper_path),
            sh_escape(endpoint),
            sh_escape(&caps),
        );
        ("osascript".to_string(), vec!["-e".to_string(), script])
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        let _ = (helper_path, endpoint, caps);
        ("true".to_string(), Vec::new())
    }
}

/// PowerShell single-quote escaping: a literal `'` is doubled inside a
/// single-quoted string.
#[cfg(target_os = "windows")]
fn ps_escape(s: &str) -> String {
    s.replace('\'', "''")
}

/// Quote a field for `do shell script`, which is **two** nested layers:
/// AppleScript parses the `"…"` literal, then hands the result to
/// `/bin/sh -c` — as root, under `with administrator privileges`.
///
/// The previous version escaped only `\` and `"`. That is correct for
/// the AppleScript literal and does nothing for the shell underneath, so
/// `$`, backtick, `;`, `|`, `&` and whitespace passed straight through
/// to a root shell. The interpolated socket path is built from
/// `$XDG_RUNTIME_DIR`, which any same-user process can set for a
/// GUI-launched app via `launchctl setenv` — turning the ordinary
/// admin-password prompt into root code execution. The helper path has
/// the same shape, which is also why an app bundle installed under a
/// path containing a space used to run the wrong program.
///
/// So: POSIX-quote first (wrap in `'…'`, rendering `'` as `'\''`), then
/// apply the AppleScript escaping to that result.
#[cfg(target_os = "macos")]
fn sh_escape(s: &str) -> String {
    let posix = format!("'{}'", s.replace('\'', r"'\''"));
    posix.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Undo the AppleScript-literal layer so a test can assert on what
    /// `/bin/sh` actually receives. AppleScript turns `\\` back into
    /// `\` and `\"` into `"`. Decoding in one pass, rather than two
    /// chained `replace` calls, avoids re-decoding the output of the
    /// first pass.
    #[cfg(target_os = "macos")]
    fn applescript_decode(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some(next @ ('\\' | '"')) => out.push(next),
                Some(next) => {
                    out.push('\\');
                    out.push(next);
                }
                None => out.push('\\'),
            }
        }
        out
    }

    /// `do shell script` runs its argument through `/bin/sh` as root, so
    /// every shell metacharacter has to be neutralised — not just the two
    /// AppleScript-literal ones. A path containing `$(...)` must survive
    /// as literal text.
    #[cfg(target_os = "macos")]
    #[test]
    fn sh_escape_neutralises_shell_metacharacters() {
        let hostile = "/tmp/$(touch /tmp/pwned)/sock";
        let out = sh_escape(hostile);
        // Wrapped in a POSIX single-quoted literal, so `$(` is inert.
        assert!(out.starts_with('\''), "must be single-quoted: {out}");
        assert!(out.ends_with('\''), "must be single-quoted: {out}");
        assert!(
            out.contains("$(touch /tmp/pwned)"),
            "content is preserved verbatim inside the quotes: {out}"
        );

        // An embedded single quote must not be able to close the literal.
        //
        // The result carries TWO layers: the POSIX `'\''` idiom for
        // /bin/sh, and then the AppleScript escaping applied on top,
        // which doubles that backslash. Asserting on the raw string
        // therefore has to look for `'\\''` — checking for the
        // single-backslash form tested the intermediate value, not what
        // `sh_escape` returns.
        let with_quote = sh_escape("/tmp/it's/sock");
        assert_eq!(
            with_quote, r"'/tmp/it'\\''s/sock'",
            "both escaping layers must be present"
        );

        // And the layer that matters: after AppleScript unescapes it,
        // /bin/sh must see a well-formed single-quoted literal whose
        // embedded quote closes and reopens rather than terminating it.
        assert_eq!(
            applescript_decode(&with_quote),
            r"'/tmp/it'\''s/sock'",
            "the shell must receive the POSIX-quoted form"
        );

        // Spaces stay inside one argument rather than splitting it.
        let spaced = sh_escape("/Applications/My App.app/helper");
        assert!(spaced.starts_with('\'') && spaced.ends_with('\''));
    }

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

    /// The endpoint the caller passes is platform-shaped: a pipe name
    /// on Windows, a loopback port on Unix.
    fn endpoint_for_host() -> &'static str {
        if cfg!(windows) {
            "freally-helper-pipe-0123456789abcdef"
        } else {
            "49871"
        }
    }

    #[test]
    fn build_spawn_command_carries_pipe_and_capability() {
        let endpoint = endpoint_for_host();
        let (program, args) = build_spawn_command(
            "/opt/freally/freally-helper",
            endpoint,
            &[Capability::ElevatedRetry],
        );
        let joined = args.join(" ");
        assert!(!program.is_empty(), "program must be set");
        assert!(
            joined.contains(endpoint),
            "the endpoint must be forwarded: {joined}"
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

    /// The Unix spawn must never name a filesystem rendezvous.
    ///
    /// `--socket=<path>` was the vulnerability: a process running as
    /// the same uid unlinks that node and binds its own while the
    /// consent dialog is up, so the helper — root by then — connects
    /// to the attacker and takes instructions. A port names a listener
    /// the parent already holds and cannot be taken from it.
    #[cfg(all(unix, not(windows)))]
    #[test]
    fn unix_spawn_rendezvouses_on_a_port_not_a_path() {
        let (_program, args) = build_spawn_command(
            "/opt/freally/freally-helper",
            "49871",
            &[Capability::ElevatedRetry],
        );
        let joined = args.join(" ");
        // macOS POSIX-quotes every field before the AppleScript layer,
        // so the port arrives as --port=(quote)49871(quote) there and bare
        // on Linux. Both are correct; asserting only the Linux shape made
        // this fail on the one platform that cannot be run locally.
        assert!(
            joined.contains("--port=49871") || joined.contains("--port='49871'"),
            "must pass the port: {joined}"
        );
        assert!(
            !joined.contains("--socket="),
            "must not rendezvous on a filesystem path: {joined}"
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
