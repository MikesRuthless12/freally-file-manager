//! `overlay-lint` — every overlay must opt into Escape-to-close.
//!
//! `lib/a11y.ts`'s `escapeToClose` exists because the older pattern — an
//! `onkeydown` on the dialog element plus `tabindex={-1}` — only fires
//! while focus is *inside* the dialog. Tab past the last control, or
//! click something that moves focus to `document.body`, and Escape
//! silently stops working. The action listens on `window` instead.
//!
//! `escapeToClose` already stamps `data-escape-closes="true"` on its
//! node so a test could assert an overlay opted in, but nothing ever
//! consumed it — which is exactly how TaskCenter and SyncDrawer shipped
//! without it. This lint is that missing consumer, at build time rather
//! than behind an e2e run.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// Overlays that deliberately do not close on Escape, with the reason.
/// Anything else growing a dialog role has to opt in or be listed here.
const EXEMPT: &[(&str, &str)] = &[
    (
        "ErrorPromptDrawer.svelte",
        "non-modal corner panel rendered in parallel with the UI — its own \
         header documents no aria-modal, no focus trap, no Esc-to-skip",
    ),
    (
        "EulaGate.svelte",
        "blocking first-run gate — dismissing it with Escape would let the \
         main UI through without acceptance",
    ),
];

/// Markers that make a component an overlay for this lint's purposes.
const OVERLAY_MARKERS: &[&str] = &["class=\"backdrop\"", "class=\"drawer\"", "role=\"dialog\""];

pub(crate) fn run() -> Result<(), String> {
    let root = crate::repo_root().ok_or("could not locate the repository root")?;
    let dir = root.join("apps/freally-ui/src/lib/components");
    if !dir.is_dir() {
        return Err(format!("components directory not found: {}", dir.display()));
    }

    let exempt: BTreeSet<&str> = EXEMPT.iter().map(|(f, _)| *f).collect();
    let mut offenders: Vec<String> = Vec::new();
    let mut checked = 0usize;
    let mut opted_in = 0usize;
    let mut unused_exemptions: Vec<&str> = Vec::new();

    let mut entries: Vec<_> = fs::read_dir(&dir)
        .map_err(|e| format!("reading {}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "svelte"))
        .collect();
    entries.sort();

    let mut seen_overlays: BTreeSet<String> = BTreeSet::new();

    for path in &entries {
        let name = file_name(path);
        let src =
            fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        let markup = strip_blocks(&src);
        if !OVERLAY_MARKERS.iter().any(|m| markup.contains(m)) {
            continue;
        }
        checked += 1;
        seen_overlays.insert(name.clone());
        if exempt.contains(name.as_str()) {
            continue;
        }
        // `escapeToClose` is the wanted form; a window-level keydown
        // listener is the same guarantee written by hand.
        let has_action = src.contains("escapeToClose");
        let has_window_listener = src.contains("<svelte:window") && src.contains("onkeydown");
        if has_action || has_window_listener {
            opted_in += 1;
        } else {
            offenders.push(name);
        }
    }

    for (name, _) in EXEMPT {
        if !seen_overlays.contains(*name) {
            unused_exemptions.push(name);
        }
    }

    if !unused_exemptions.is_empty() {
        return Err(format!(
            "stale exemption(s) in xtask/src/overlay.rs — no longer an overlay (or renamed): {}",
            unused_exemptions.join(", ")
        ));
    }

    if !offenders.is_empty() {
        let mut msg = String::from(
            "overlay(s) missing Escape-to-close. Add `use:escapeToClose={<close fn>}`\n\
             from `src/lib/a11y.ts` to the overlay's root element — a node-level\n\
             `onkeydown` only fires while focus is already inside the dialog.\n\
             If the overlay genuinely must not close on Escape, add it to EXEMPT\n\
             in xtask/src/overlay.rs with the reason.\n\n",
        );
        for o in &offenders {
            msg.push_str(&format!("  - {o}\n"));
        }
        return Err(msg);
    }

    println!(
        "overlay-lint: OK — {checked} overlays ({opted_in} opted in, {} exempt)",
        EXEMPT.len()
    );
    Ok(())
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Drop `<script>` / `<style>` bodies so a marker mentioned in code or a
/// selector cannot make a non-overlay look like one.
fn strip_blocks(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    loop {
        let script = rest.find("<script");
        let style = rest.find("<style");
        let (start, close_tag) = match (script, style) {
            (Some(a), Some(b)) if a < b => (a, "</script>"),
            (Some(_), Some(b)) => (b, "</style>"),
            (Some(a), None) => (a, "</script>"),
            (None, Some(b)) => (b, "</style>"),
            (None, None) => break,
        };
        out.push_str(&rest[..start]);
        match rest[start..].find(close_tag) {
            Some(end) => rest = &rest[start + end + close_tag.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_script_and_style_bodies() {
        let src = "<script>const a = 'role=\"dialog\"';</script>\n<div class=\"x\"></div>\n<style>.backdrop{}</style>";
        let out = strip_blocks(src);
        assert!(!out.contains("role=\"dialog\""));
        assert!(!out.contains(".backdrop{}"));
        assert!(out.contains("class=\"x\""));
    }

    #[test]
    fn markup_outside_blocks_survives() {
        let src = "<script>let x = 1;</script>\n<div class=\"backdrop\"></div>";
        assert!(strip_blocks(src).contains("class=\"backdrop\""));
    }

    #[test]
    fn unterminated_script_does_not_panic() {
        assert_eq!(strip_blocks("<script>oops"), "");
    }
}
