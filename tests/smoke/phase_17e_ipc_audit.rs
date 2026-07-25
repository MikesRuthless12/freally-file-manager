//! Phase 17e smoke test — IPC argument audit + canonicalisation.
//!
//! Asserts that:
//!
//! 1. `crate::ipc_safety::validate_ipc_path` rejects traversal,
//!    NUL, empty, and U+FFFD-bearing strings; returns a clean
//!    `PathBuf` on a normal input.
//! 2. The `IpcError::localized_key` map is stable — frontends pin
//!    against the Fluent keys, never the variant names.
//! 3. The `err-path-invalid-encoding` key is present in every
//!    locale (cross-checked indirectly via the existence count).
//! 4. **Source-grep tripwire** — every `#[tauri::command]` whose
//!    signature contains a `path:`/`paths:`/`destination:`/`source:`
//!    arg in `apps/freally-ui/src-tauri/src/commands.rs` calls
//!    one of the gate helpers. Drift past this gate fires before
//!    the commit lands.

use std::fs;
use std::path::{Path, PathBuf};

use freally_ui_lib::ipc_safety::{IpcError, validate_ipc_path, validate_ipc_paths};

#[test]
fn rejects_traversal_via_helper() {
    assert_eq!(
        validate_ipc_path("foo/../etc/passwd"),
        Err(IpcError::PathEscape)
    );
}

#[test]
fn rejects_empty_via_helper() {
    assert_eq!(validate_ipc_path("   "), Err(IpcError::EmptyPath));
}

#[test]
fn rejects_replacement_character_via_helper() {
    let bad = format!("good{}/path", '\u{FFFD}');
    assert_eq!(validate_ipc_path(&bad), Err(IpcError::InvalidEncoding));
}

#[test]
fn accepts_normal_path() {
    let p = validate_ipc_path("/var/log/freally").unwrap();
    assert_eq!(p, PathBuf::from("/var/log/freally"));
}

#[test]
fn empty_list_is_distinct() {
    let raws: Vec<String> = vec![];
    assert_eq!(validate_ipc_paths(raws), Err(IpcError::EmptyList));
}

#[test]
fn fluent_keys_are_stable_against_drift() {
    assert_eq!(IpcError::PathEscape.localized_key(), "err-path-escape");
    assert_eq!(IpcError::EmptyPath.localized_key(), "err-destination-empty");
    assert_eq!(
        IpcError::InvalidEncoding.localized_key(),
        "err-path-invalid-encoding"
    );
    assert_eq!(IpcError::EmptyList.localized_key(), "err-source-required");
}

fn repo_root() -> PathBuf {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut cur: &Path = &here;
    loop {
        if cur.join("Cargo.toml").is_file() && cur.join("locales").is_dir() {
            return cur.to_path_buf();
        }
        cur = match cur.parent() {
            Some(p) => p,
            None => break,
        };
    }
    panic!("could not locate repo root from {}", here.display());
}

#[test]
fn every_locale_carries_the_new_key() {
    // 18-locale parity is the contract `xtask i18n-lint` enforces in
    // CI; this anchor catches drift if a developer adds the key only
    // to `en/`.
    let root = repo_root();
    let locales_dir = root.join("locales");
    let mut count = 0;
    for entry in fs::read_dir(&locales_dir).unwrap().flatten() {
        let ftl = entry.path().join("freally.ftl");
        if !ftl.exists() {
            continue;
        }
        let body = fs::read_to_string(&ftl).unwrap();
        assert!(
            body.contains("err-path-invalid-encoding ="),
            "locale {} is missing err-path-invalid-encoding",
            entry.path().display(),
        );
        count += 1;
    }
    assert_eq!(count, 18, "expected 18 locales, saw {count}");
}

/// Commands that take a path-typed argument but do not call a Phase 17e
/// gate. Every one of these PRE-DATES the FFM-M09..M16 build that
/// widened this sweep from `commands.rs` to the whole command surface;
/// auditing them is its own workstream, so they are frozen here rather
/// than silently skipped.
///
/// This list is a **ratchet**: it may shrink, never grow. Adding a new
/// ungated path-taking command fails the test, and fixing one of these
/// fails it too until the entry is removed.
const PRE_EXISTING_UNGATED: &[(&str, &str)] = &[
    ("backup_commands.rs", "sources_add"),
    ("backup_commands.rs", "sources_update"),
    ("dropstack.rs", "dropstack_remove"),
    ("dropstack.rs", "dropstack_copy_all_to"),
    ("offload_commands.rs", "smb_compression_state"),
    ("offload_commands.rs", "render_offload_template"),
    ("preview_commands.rs", "compute_tree_diff"),
    ("queue_commands.rs", "queue_route_job"),
    ("queue_commands.rs", "queue_pin_destination"),
    ("repository_commands.rs", "repository_export_report"),
    ("repository_commands.rs", "repository_create"),
    ("repository_commands.rs", "repository_connect"),
    ("repository_commands.rs", "restore_preview"),
    ("repository_commands.rs", "restore_paths"),
    ("scan_commands.rs", "scan_start"),
    ("version_commands.rs", "list_versions"),
    ("version_commands.rs", "select_versions_to_prune"),
    ("version_commands.rs", "prune_versions"),
];

#[test]
fn command_path_args_pass_through_the_gate() {
    // Tripwire — walk EVERY command module and assert that each
    // `#[tauri::command]` whose signature mentions a path-typed arg
    // (`path:`, `paths:`, `source:`, `destination:`, `src:`, `dst:`)
    // also calls one of the Phase 17e helpers (`validate_ipc_path` /
    // `validate_ipc_paths` / `validate_path_no_traversal`), or
    // delegates to a helper that does.
    //
    // Sweeping the whole directory rather than `commands.rs` alone is
    // deliberate: the FFM-M09..M16 build added 12 path-taking commands
    // across 8 new modules, none of which a single-file sweep could
    // see.
    let src_dir = repo_root().join("apps/freally-ui/src-tauri/src");
    let suspicious_kw = ["path:", "paths:", "source:", "destination:", "src:", "dst:"];
    let gate_kw = [
        "validate_ipc_path",
        "validate_ipc_paths",
        "validate_path_no_traversal",
        "ipc_safety::",
    ];

    let mut audited = 0;
    let mut ungated: Vec<(String, String)> = Vec::new();
    let mut files = 0;

    for entry in fs::read_dir(&src_dir).expect("src dir missing") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        files += 1;
        let file_name = path
            .file_name()
            .expect("file name")
            .to_string_lossy()
            .into_owned();
        let body = fs::read_to_string(&path).expect("read source");

        for block in body.split("#[tauri::command]").skip(1) {
            let sig_end = block.find('{').unwrap_or(block.len());
            let signature = &block[..sig_end];
            if !suspicious_kw.iter().any(|kw| signature.contains(kw)) {
                continue;
            }
            audited += 1;

            let has_gate = gate_kw.iter().any(|kw| block.contains(kw));
            // Some bodies delegate to an internal helper that gates.
            let delegates = block.contains("enqueue(")
                || block.contains("enqueue_jobs(")
                || block.contains("dropstack_apply_to(")
                || block.contains("dropstack::")
                // FFM-M16 — `elevate_batch_apply` re-derives its paths
                // from the history DB via the ledger command, which
                // gates them; the wire entries are only a selection.
                || block.contains("elevate_batch_ledger(");
            if has_gate || delegates {
                continue;
            }
            let fn_name = signature
                .split("fn ")
                .nth(1)
                .and_then(|rest| rest.split(['(', '<', ' ']).next())
                .unwrap_or("<unknown>")
                .to_string();
            ungated.push((file_name.clone(), fn_name));
        }
    }

    assert!(
        files > 20,
        "expected the full command surface, saw {files} files"
    );
    assert!(
        audited >= 15,
        "expected to audit at least 15 path-typed commands across the \
         whole command surface, saw {audited}"
    );

    let mut found: Vec<(String, String)> = ungated;
    found.sort();
    let mut frozen: Vec<(String, String)> = PRE_EXISTING_UNGATED
        .iter()
        .map(|(f, n)| ((*f).to_string(), (*n).to_string()))
        .collect();
    frozen.sort();

    let added: Vec<&(String, String)> = found.iter().filter(|f| !frozen.contains(f)).collect();
    assert!(
        added.is_empty(),
        "new command(s) accept a path-typed arg without calling a Phase 17e gate: {added:#?}"
    );
    let fixed: Vec<&(String, String)> = frozen.iter().filter(|f| !found.contains(f)).collect();
    assert!(
        fixed.is_empty(),
        "these commands now gate their paths — remove them from \
         PRE_EXISTING_UNGATED so the ratchet keeps tightening: {fixed:#?}"
    );
}
