//! `xtask build-sample-plugins` — compile every Phase 46.5 sample
//! plugin under `apps/copythat-ui/plugins/` to
//! `target/wasm32-unknown-unknown/release/<name>.wasm`.
//!
//! Each plugin lives in a standalone Cargo package (its own
//! `Cargo.toml` carrying an empty `[workspace]` table), so we can't
//! drive them with the host workspace's build. Instead we shell out
//! one `cargo build` per plugin from inside its directory.
//!
//! Why this command exists at all (rather than letting CI run
//! `cargo build` against each plugin separately): without a single
//! entry point that walks every plugin, a future plugin added to the
//! tree would silently miss CI coverage until someone remembered to
//! update the workflow yaml. `xtask build-sample-plugins` is the one
//! place CI calls; adding a plugin is just dropping a new directory.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::repo_root;

/// Plugin directory names under `apps/copythat-ui/plugins/`. Listed
/// in the order Phase 46.5's spec introduces them so the CI log
/// reads top-to-bottom in the same order as the build prompts guide.
const SAMPLE_PLUGINS: &[&str] = &[
    "organize-by-exif",
    "notify-discord",
    "notify-ntfy",
    "dedup-warning",
];

const PLUGIN_TARGET: &str = "wasm32-unknown-unknown";

pub fn run() -> Result<(), String> {
    let root = repo_root().ok_or("could not locate repo root (Cargo.toml + locales/)")?;
    let plugins_dir = root.join("apps").join("copythat-ui").join("plugins");
    if !plugins_dir.is_dir() {
        return Err(format!(
            "expected sample-plugin root at {}, but the directory does not exist",
            plugins_dir.display()
        ));
    }

    ensure_wasm_target_installed()?;

    let mut built: Vec<(String, PathBuf)> = Vec::with_capacity(SAMPLE_PLUGINS.len());
    for name in SAMPLE_PLUGINS {
        let dir = plugins_dir.join(name);
        let wasm = build_one(&dir, name)?;
        built.push(((*name).to_string(), wasm));
    }

    println!(
        "build-sample-plugins: OK ({} plugin{})",
        built.len(),
        if built.len() == 1 { "" } else { "s" }
    );
    for (name, path) in &built {
        // Render the path relative to the repo root so the CI log
        // doesn't leak the absolute checkout location.
        let rel = path.strip_prefix(&root).unwrap_or(path);
        println!("  {name:<24} {}", rel.display());
    }
    Ok(())
}

/// Verify `wasm32-unknown-unknown` is installed via rustup and emit
/// a clear instruction if it is not. We don't auto-install — that
/// requires network access and the failure message is more helpful
/// than a silent download anyway.
fn ensure_wasm_target_installed() -> Result<(), String> {
    let output = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .map_err(|e| format!("running `rustup target list --installed`: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "`rustup target list` failed with status {}",
            output.status
        ));
    }
    let installed = String::from_utf8_lossy(&output.stdout);
    if installed.lines().any(|l| l.trim() == PLUGIN_TARGET) {
        return Ok(());
    }
    Err(format!(
        "{PLUGIN_TARGET} target is not installed. Run `rustup target add {PLUGIN_TARGET}` and retry."
    ))
}

/// Compile one plugin, then locate the produced `.wasm` artifact in
/// its `target/wasm32-unknown-unknown/release/` directory and copy
/// the source-tree `plugin.toml` next to it. Cargo names cdylib
/// outputs after the package name with hyphens translated to
/// underscores (Rust crate convention), so we don't hard-code the
/// filename — we look for the only `.wasm` in the release
/// directory.
///
/// Why we copy `plugin.toml` here (rather than leaving it where the
/// source lives): the host's `PluginHost::load_plugin` reads the
/// manifest from the directory adjacent to the `.wasm`, so a
/// "shippable" plugin is the (`.wasm`, `plugin.toml`) pair sitting
/// in the same directory. Mirrors how the per-user installer in
/// 46.6 will lay plugins out.
fn build_one(dir: &Path, name: &str) -> Result<PathBuf, String> {
    println!("build-sample-plugins: compiling {name}…");
    let status = Command::new("cargo")
        .current_dir(dir)
        .args(["build", "--target", PLUGIN_TARGET, "--release"])
        .status()
        .map_err(|e| format!("spawning cargo for {name}: {e}"))?;
    if !status.success() {
        return Err(format!("cargo build failed for {name} (status {status})"));
    }
    let release_dir = dir
        .join("target")
        .join(PLUGIN_TARGET)
        .join("release");
    let mut wasms: Vec<PathBuf> = std::fs::read_dir(&release_dir)
        .map_err(|e| format!("reading {}: {e}", release_dir.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("wasm"))
        .collect();
    if wasms.is_empty() {
        return Err(format!(
            "{name}: cargo reported success but no .wasm found in {}",
            release_dir.display()
        ));
    }
    if wasms.len() > 1 {
        return Err(format!(
            "{name}: expected exactly one .wasm in {} (found {})",
            release_dir.display(),
            wasms.len()
        ));
    }
    let wasm = wasms.remove(0);

    let manifest_src = dir.join("plugin.toml");
    let manifest_dst = release_dir.join("plugin.toml");
    std::fs::copy(&manifest_src, &manifest_dst).map_err(|e| {
        format!(
            "{name}: copying {} -> {}: {e}",
            manifest_src.display(),
            manifest_dst.display()
        )
    })?;

    Ok(wasm)
}
