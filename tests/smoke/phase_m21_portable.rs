//! FFM-M21 — a portable install keeps its state beside the binary.
//!
//! Covers the resolvers that used to reach `directories::ProjectDirs`
//! unconditionally (history DB, resume journal, chunk store, scan
//! index, audit log, drop-stack, plugin dir, thumbnail cache). Each now
//! consults `portable::portable_root()` first and falls back to its
//! original OS path, so a normal install is byte-identical to before.
//!
//! This drives the real `freally` CLI in a child process on purpose:
//! `portable_root()` is a process-wide `OnceLock` seeded from
//! `current_exe()`, so portable and non-portable cannot both be
//! exercised inside one test binary. Staging the binary in a tempdir is
//! also what a portable install actually looks like.
//!
//! Self-skips when the CLI has not been built, matching the convention
//! the §4.10 CLI specs use.

use std::path::PathBuf;
use std::process::Command;

fn cli_binary() -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        "freally.exe"
    } else {
        "freally"
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..");
    ["debug", "release"]
        .iter()
        .map(|p| root.join("target").join(p).join(exe))
        .find(|p| p.is_file())
}

fn stage(tmp: &std::path::Path) -> Option<PathBuf> {
    let bin = cli_binary()?;
    let staged = tmp.join(bin.file_name().expect("binary filename"));
    std::fs::copy(&bin, &staged).expect("stage the CLI binary");
    Some(staged)
}

#[test]
fn portable_run_writes_history_beside_the_binary() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let Some(staged) = stage(tmp.path()) else {
        eprintln!("freally CLI not built - skipping (cargo build -p freally-cli)");
        return;
    };

    let out = Command::new(&staged)
        .arg("history")
        .env("FREALLY_PORTABLE", "1")
        .current_dir(tmp.path())
        .output()
        .expect("run the staged CLI");
    assert!(
        out.status.success(),
        "portable run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let db = tmp.path().join("FreallyData").join("history.db");
    assert!(
        db.is_file(),
        "portable run left no history DB at {}",
        db.display()
    );
}

#[test]
fn non_portable_run_leaves_the_binary_directory_clean() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let Some(staged) = stage(tmp.path()) else {
        eprintln!("freally CLI not built - skipping (cargo build -p freally-cli)");
        return;
    };

    let out = Command::new(&staged)
        .arg("history")
        .env_remove("FREALLY_PORTABLE")
        .current_dir(tmp.path())
        .output()
        .expect("run the staged CLI");
    assert!(
        out.status.success(),
        "non-portable run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        !tmp.path().join("FreallyData").exists(),
        "a non-portable install must not create FreallyData beside the binary"
    );
}
