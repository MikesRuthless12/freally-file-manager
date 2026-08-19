//! Opt-in, anonymous bug reporting — **no telemetry, nothing auto-sends,
//! no server we run, no credentials shipped.**
//!
//! Ported from the Havoc reference implementation (Freally Capture's
//! `src-tauri/src/bugreport.rs`). A panic hook writes a **scrubbed**
//! crash report to a local file; the next launch offers to report it.
//! The dialog shows the user the **exact** text that would be sent and
//! submits it only on an explicit click, via a pre-filled GitHub issue,
//! Gmail's web composer, or their own mail client.
//!
//! What a report carries: the app version, OS and arch, the user's own
//! description, and (optionally) a crash excerpt with the home path and
//! username redacted. Never file contents, paths the user copied,
//! credentials, or repository data.
//!
//! # Two deliberate departures from the reference
//!
//! 1. **No `--crash-notice` relaunch helper.** Capture spawns itself as
//!    a helper that shows a native message box and relaunches, because a
//!    dying audio studio cannot render its own error window. That helper
//!    pulls in `rfd` (with carefully pinned GTK features) and races the
//!    single-instance guard — and this app already uses
//!    `tauri-plugin-single-instance` for `--enqueue` shell dispatch.
//!    Bolting it on unvalidated risks breaking the shell integration, so
//!    the hook here writes the file and lets the next launch surface it.
//! 2. **Portable-aware crash directory.** Reports follow FFM-M21: on a
//!    portable install they live beside the binary, not in the host's
//!    data dir, so a crash report never outlives the stick.

use std::path::PathBuf;

use serde::Serialize;

/// Named in the subject line so a report landing in the shared inbox is
/// instantly attributable to the right Havoc app.
const APP_NAME: &str = "Freally File Manager";
/// Pre-filled issue URL. The user submits it while signed in — no token.
const GITHUB_NEW_ISSUE: &str = "https://github.com/MikesRuthless12/freally-file-manager/issues/new";
/// Where an emailed report goes — sent by the user's own mail client.
const REPORT_EMAIL: &str = "mythodikalone@gmail.com";
/// Gmail web compose. Plain https, no API key; nothing sends until the
/// user clicks Send. Offered *alongside* `mailto:` for non-Gmail users.
const GMAIL_COMPOSE: &str = "https://mail.google.com/mail/?view=cm&fs=1";

/// Bounds on the **percent-encoded** body. A character cap cannot bound
/// a URL: one 3-byte character (`—`) encodes to nine. Browsers take
/// ~32 k, so the https targets are generous.
const MAX_GITHUB_ENCODED: usize = 6000;
const MAX_GMAIL_ENCODED: usize = 6000;
/// The whole `mailto:` URL stays under this. Windows' ShellExecute dies
/// near 2048 and then opens a blank window with no error.
const MAX_MAILTO_URL: usize = 1900;
/// …of which the subject may claim at most this, so a pathological
/// subject cannot starve the body of every byte.
const MAX_MAILTO_SUBJECT_ENCODED: usize = 300;

/// Where crash reports live.
///
/// Portable installs keep them beside the binary (FFM-M21) so nothing is
/// left on the host; a normal install uses the OS data dir.
fn crash_dir() -> Option<PathBuf> {
    if let Some(root) = freally_settings::portable::portable_root() {
        return Some(root.join("crash-reports"));
    }
    directories::ProjectDirs::from("com", "Freally", "freally-file-manager")
        .map(|d| d.data_dir().join("crash-reports"))
}

/// Redact the OS user's home path and username so a report carries no
/// personal identifier.
///
/// The report is always shown to the user before it can be sent, so
/// over-redaction is safe and under-redaction is visible.
pub fn scrub(text: &str) -> String {
    let mut out = text.to_string();
    if let Some(dirs) = directories::UserDirs::new() {
        let home = dirs.home_dir().to_string_lossy().to_string();
        if !home.is_empty() {
            out = out.replace(&home, "<home>");
            // The bare username too, unless it is a trivially short
            // substring that would corrupt unrelated words.
            if let Some(name) = std::path::Path::new(&home)
                .file_name()
                .and_then(|n| n.to_str())
                && name.len() >= 3
            {
                out = out.replace(name, "<user>");
            }
        }
    }
    out
}

/// The always-anonymous system line.
pub fn diagnostics() -> String {
    format!(
        "App: {APP_NAME} {}\nOS: {} / {}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

/// Install the panic hook. Call once at startup.
///
/// A panic writes a scrubbed crash report next to the previous ones,
/// then the previous hook runs so normal panic output is unchanged.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|loc| format!("{}:{}", loc.file(), loc.line()))
            .unwrap_or_else(|| "unknown".to_string());
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(no message)".to_string());
        let backtrace = std::backtrace::Backtrace::force_capture();
        let raw = format!("Panic at {location}\nMessage: {message}\n\nBacktrace:\n{backtrace}\n");
        write_crash(&scrub(&raw));
        previous(info);
    }));
}

/// Local + UTC stamp for a crash excerpt.
///
/// The local offset narrows the reporter to a timezone, which is weakly
/// identifying. It is included because a crash reported days later is
/// otherwise impossible to order — and the user reads the exact text
/// before anything is sent. Nothing finer (locale, hostname, IP) is
/// collected.
fn crash_time_line() -> String {
    let now = chrono::Local::now();
    format!(
        "When: {} (local, UTC{}) / {} UTC",
        now.format("%Y-%m-%d %H:%M:%S"),
        now.format("%:z"),
        now.naive_utc().format("%Y-%m-%d %H:%M:%S"),
    )
}

fn write_crash(scrubbed: &str) {
    let Some(dir) = crash_dir() else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let stamped = format!("{}\n{scrubbed}", crash_time_line());
    let _ = std::fs::write(dir.join(format!("crash-{ts}.txt")), stamped);
}

/// The newest pending crash report (already scrubbed), if any.
pub fn pending_crash() -> Option<String> {
    let dir = crash_dir()?;
    let mut newest: Option<(u128, PathBuf)> = None;
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("txt") {
            continue;
        }
        let mtime = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis())
            .unwrap_or(0);
        if newest.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
            newest = Some((mtime, path));
        }
    }
    let (_, path) = newest?;
    std::fs::read_to_string(path).ok()
}

/// Delete pending crash reports — the user sent or dismissed them.
pub fn clear_crashes() {
    if let Some(dir) = crash_dir() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Percent-encode a query component (RFC 3986 unreserved kept verbatim).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for byte in s.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("\n… (truncated — use “Copy report” for the full text)");
    out
}

/// Percent-encode `s` so the result never exceeds `max_encoded` bytes,
/// appending `note` (itself encoded, and reserved out of the budget)
/// when anything was cut. Whole encoded characters only — never half of
/// a `%E2%80%94`.
fn encode_bounded_with(s: &str, max_encoded: usize, note: &str) -> String {
    let full = urlencode(s);
    if full.len() <= max_encoded {
        return full;
    }
    let note = urlencode(note);
    let budget = max_encoded.saturating_sub(note.len());
    let mut out = String::with_capacity(max_encoded);
    let mut buf = [0u8; 4];
    for ch in s.chars() {
        let piece = urlencode(ch.encode_utf8(&mut buf));
        if out.len() + piece.len() > budget {
            break;
        }
        out.push_str(&piece);
    }
    out.push_str(&note);
    out
}

fn encode_bounded(s: &str, max_encoded: usize) -> String {
    encode_bounded_with(
        s,
        max_encoded,
        "\n… (truncated — use “Copy report” for the full text)",
    )
}

/// [`encode_bounded`] without the note — for a subject line, where a
/// trailing sentence would be absurd.
fn encode_capped(s: &str, max_encoded: usize) -> String {
    encode_bounded_with(s, max_encoded, "")
}

/// A pre-filled GitHub "new issue" URL.
pub fn github_url(title: &str, body: &str) -> String {
    format!(
        "{GITHUB_NEW_ISSUE}?labels=bug&title={}&body={}",
        urlencode(&truncate_chars(title, 200)),
        encode_bounded(body, MAX_GITHUB_ENCODED),
    )
}

/// A pre-filled `mailto:` URL.
///
/// The bound is on the **whole URL**, not the body alone: the scheme,
/// address and subject are user-influenced too, and 80 CJK characters
/// encode to 720 bytes. Without this a non-English report could cross
/// the ~2048 mark where ShellExecute opens a blank window and reports
/// no error.
pub fn mailto_url(subject: &str, body: &str) -> String {
    let head = format!(
        "mailto:{REPORT_EMAIL}?subject={}&body=",
        encode_capped(&truncate_chars(subject, 200), MAX_MAILTO_SUBJECT_ENCODED),
    );
    let budget = MAX_MAILTO_URL.saturating_sub(head.len());
    format!("{head}{}", encode_bounded(body, budget))
}

/// A pre-filled Gmail web-compose URL. Unlike `mailto:` this never
/// depends on a registered mail handler.
pub fn gmail_url(subject: &str, body: &str) -> String {
    format!(
        "{GMAIL_COMPOSE}&to={}&su={}&body={}",
        urlencode(REPORT_EMAIL),
        urlencode(&truncate_chars(subject, 200)),
        encode_bounded(body, MAX_GMAIL_ENCODED),
    )
}

/// Open an https/mailto URL with the OS default handler.
///
/// The URL is one we built and is passed as a single argv entry — no
/// shell. The scheme allowlist refuses `file:` / `javascript:`.
fn open_url(url: &str) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("mailto:")) {
        return Err("refusing to open a non-https/mailto URL".into());
    }
    if url.chars().any(char::is_control) {
        return Err("invalid URL".into());
    }
    #[cfg(target_os = "windows")]
    {
        // Absolute path, never the bare name. Rust's Windows `Command`
        // resolution searches the directory of `current_exe()` *before*
        // System32, and this module ships in portable installs — which
        // live on a stick, a share, or a synced folder, anywhere a
        // second party can drop one `rundll32.exe`. Same reasoning and
        // same shape as `elevate::absolute_powershell` and
        // `freally_platform::scheduler::os_tool`.
        let rundll32 = match std::env::var("SystemRoot") {
            Ok(root) if !root.is_empty() => format!(r"{root}\System32\rundll32.exe"),
            _ => r"C:\Windows\System32\rundll32.exe".to_string(),
        };
        std::process::Command::new(rundll32)
            .args(["url.dll,FileProtocolHandler", url])
            .spawn()
            .map_err(|err| format!("could not open the link: {err}"))?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map_err(|err| format!("could not open the link: {err}"))?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map_err(|err| format!("could not open the link: {err}"))?;
    }
    Ok(())
}

/// One short line naming what went wrong, for the subject.
///
/// Prefers the panic message, then the user's first non-empty line, so
/// a report is attributable at a glance in the shared inbox.
fn error_summary(crash: Option<&str>, description: &str) -> String {
    let from_crash = crash.and_then(|c| {
        c.lines()
            .find_map(|line| line.strip_prefix("Message: "))
            .map(str::to_string)
    });
    from_crash
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            description
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_string)
        })
        .map(|s| truncate_chars(&s, 80))
        .unwrap_or_else(|| {
            if crash.is_some() {
                "crash report".to_string()
            } else {
                "bug report".to_string()
            }
        })
}

/// The exact text the user is shown and that gets submitted.
fn report_body(description: &str, crash: Option<&str>) -> String {
    let mut out = String::new();
    out.push_str(&diagnostics());
    out.push_str("\n## What happened\n\n");
    if description.trim().is_empty() {
        out.push_str("(no description provided)\n");
    } else {
        out.push_str(&scrub(description));
        out.push('\n');
    }
    if let Some(c) = crash {
        out.push_str("\n## Crash excerpt (scrubbed)\n\n```\n");
        out.push_str(c);
        out.push_str("\n```\n");
    }
    out
}

/// What the Settings pane needs to render the dialog.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BugReportContextDto {
    /// Anonymous system line, always shown.
    pub diagnostics: String,
    /// The scrubbed crash excerpt awaiting a decision, if any.
    pub pending_crash: Option<String>,
}

/// `bug_report_context()` — diagnostics + any pending crash.
#[tauri::command]
pub fn bug_report_context() -> BugReportContextDto {
    BugReportContextDto {
        diagnostics: diagnostics(),
        pending_crash: pending_crash(),
    }
}

/// `bug_report_preview(description, includeCrash)` — the exact text that
/// would be submitted, so the dialog can show it verbatim.
#[tauri::command]
pub fn bug_report_preview(description: String, include_crash: bool) -> String {
    let crash = if include_crash { pending_crash() } else { None };
    report_body(&description, crash.as_deref())
}

/// `bug_report_submit(description, includeCrash, channel)` — open the
/// chosen channel pre-filled. Nothing is transmitted by this app; the
/// user still presses Send.
#[tauri::command]
pub fn bug_report_submit(
    description: String,
    include_crash: bool,
    channel: String,
) -> Result<(), String> {
    let crash = if include_crash { pending_crash() } else { None };
    let body = report_body(&description, crash.as_deref());
    // Names the app AND the error, so a report is attributable the
    // moment it lands in the shared inbox.
    // Scrub the description here too. `report_body` already does, but
    // the subject was built from the raw text, so a user who typed a
    // path containing their username shipped it in the GitHub issue
    // title / mail subject while the preview showed `<home>`.
    let subject = format!(
        "[{APP_NAME}] {}",
        error_summary(crash.as_deref(), &scrub(&description))
    );
    let url = match channel.as_str() {
        "github" => github_url(&subject, &body),
        "gmail" => gmail_url(&subject, &body),
        "email" => mailto_url(&subject, &body),
        other => return Err(format!("unknown report channel `{other}`")),
    };
    open_url(&url)
}

/// `bug_report_clear_crash()` — the user dismissed the pending crash.
#[tauri::command]
pub fn bug_report_clear_crash() {
    clear_crashes();
}

/// `bug_report_simulate()` — write a synthetic crash report without
/// crashing, so the report flow can be exercised on demand.
///
/// The reference also ships a `--test-crash` argv flag that really
/// aborts; that is deliberately omitted here — see the module docs on
/// the crash-notice helper.
#[tauri::command]
pub fn bug_report_simulate() {
    write_crash(&scrub(
        "Panic at simulated\nMessage: simulated crash report (no crash occurred)\n",
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_removes_home_and_username() {
        let Some(dirs) = directories::UserDirs::new() else {
            return; // no home on this host; nothing to assert
        };
        let home = dirs.home_dir().to_string_lossy().to_string();
        if home.is_empty() {
            return;
        }
        let text = format!("failed to open {home}/secret.txt");
        let out = scrub(&text);
        assert!(!out.contains(&home), "home path survived scrubbing: {out}");
        assert!(out.contains("<home>"), "expected a <home> marker: {out}");
    }

    #[test]
    fn mailto_url_stays_under_the_shell_execute_limit() {
        // A body of multi-byte characters is the case a character-count
        // cap gets wrong: each one encodes to nine bytes.
        let body = "—".repeat(4000);
        let subject = "[Freally File Manager] 日本語のとても長い件名".repeat(20);
        let url = mailto_url(&subject, &body);
        assert!(
            url.len() <= MAX_MAILTO_URL,
            "mailto URL was {} bytes, over the {MAX_MAILTO_URL} bound",
            url.len()
        );
    }

    #[test]
    fn encoding_never_splits_a_multibyte_escape() {
        let body = "—".repeat(500);
        let encoded = encode_bounded(&body, 100);
        // Every escape is a complete %XX triplet.
        let mut chars = encoded.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '%' {
                let a = chars.next();
                let b = chars.next();
                assert!(
                    a.is_some_and(|c| c.is_ascii_hexdigit())
                        && b.is_some_and(|c| c.is_ascii_hexdigit()),
                    "truncated mid-escape: {encoded}"
                );
            }
        }
    }

    #[test]
    fn subject_names_the_app_and_the_error() {
        let crash = "When: …\nPanic at src/x.rs:1\nMessage: index out of bounds\n";
        let summary = error_summary(Some(crash), "");
        assert_eq!(summary, "index out of bounds");
        // Falls back to the user's first line when there is no crash.
        assert_eq!(
            error_summary(None, "  \nit froze on copy\n"),
            "it froze on copy"
        );
        // …and to a stable label when there is neither.
        assert_eq!(error_summary(None, ""), "bug report");
    }

    #[test]
    fn open_url_refuses_non_https_schemes() {
        assert!(open_url("file:///etc/passwd").is_err());
        assert!(open_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn report_body_scrubs_the_user_description() {
        let Some(dirs) = directories::UserDirs::new() else {
            return;
        };
        let home = dirs.home_dir().to_string_lossy().to_string();
        if home.is_empty() {
            return;
        }
        let body = report_body(&format!("it broke on {home}/x"), None);
        assert!(
            !body.contains(&home),
            "description was not scrubbed: {body}"
        );
    }
}
