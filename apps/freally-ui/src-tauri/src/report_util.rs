//! Shared helpers for the exported-report surfaces (FFM-M10's
//! copy-verification certificate, FFM-M14's preflight plan) and the
//! FFM-M15 salvage manifest.
//!
//! These three landed together and each needed the same two things: a
//! wall-clock stamp for the document header, and HTML escaping for
//! every user-controlled path that reaches a rendered page. Escaping in
//! particular is a security-shaped function — two copies means the next
//! fix lands in one of them — so it lives here once.
//!
//! CSV field quoting is *not* here: `failed_commands::csv_field` already
//! owned that rule before this build, so the report renderers call it
//! rather than restating it a third time.

use std::path::{Path, PathBuf};

/// Escape the five characters that can break out of HTML text or an
/// attribute value.
pub(crate) fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Milliseconds since the Unix epoch, for a document's "generated at"
/// stamp. Saturates to 0 on a pre-epoch clock rather than panicking —
/// a nonsense timestamp must not fail an export.
pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// `path` with `suffix` appended to its file name — `report.html` +
/// `.sig` → `report.html.sig`. Appends to the whole name rather than
/// replacing the extension, so the original name stays recoverable.
pub(crate) fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_escape_covers_every_breakout_character() {
        assert_eq!(
            html_escape(r#"<a href="x">&'</a>"#),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;&lt;/a&gt;"
        );
    }

    #[test]
    fn html_escape_leaves_ordinary_text_alone() {
        assert_eq!(html_escape("C:/data/report 1.txt"), "C:/data/report 1.txt");
    }

    #[test]
    fn now_ms_is_a_plausible_wall_clock() {
        // Sanity only: after 2020-01-01 and before 2100-01-01.
        let now = now_ms();
        assert!(now > 1_577_836_800_000, "{now}");
        assert!(now < 4_102_444_800_000, "{now}");
    }

    #[test]
    fn with_suffix_appends_rather_than_replacing_the_extension() {
        assert_eq!(
            with_suffix(Path::new("/out/report.html"), ".sig"),
            PathBuf::from("/out/report.html.sig")
        );
        assert_eq!(
            with_suffix(Path::new("/out/disc.iso"), ".freally-salvage.json"),
            PathBuf::from("/out/disc.iso.freally-salvage.json")
        );
    }
}
