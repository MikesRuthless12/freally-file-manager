//! Stage the privileged helper as a Tauri sidecar.
//!
//! `freally-helper` is a separate binary that holds every elevated path
//! the UI process must never touch: shell-extension install/uninstall,
//! hardware secure-erase, and elevated-retry on a permission-denied
//! copy. `elevate::sibling_helper` looks for it next to the running
//! executable and refuses to elevate when it is not there.
//!
//! Nothing was putting it there. `tauri.conf.json` declared no
//! `externalBin` and no `resources`, and the release workflow never
//! built the crate, so the shipped installers contained exactly one
//! file — `freally-ui.exe` — and every elevated path failed with
//! "helper binary not found" in the field while working fine from a dev
//! checkout, where the two binaries happen to share `target/debug/`.
//!
//! Tauri resolves `externalBin` entries by appending the target triple,
//! so the file it looks for is
//! `binaries/freally-helper-<triple>[.exe]`. This builds the helper for
//! that triple and puts it there. It is a build input, not a source
//! file: `binaries/` is git-ignored and regenerated on every build.

use std::path::{Path, PathBuf};
use std::process::Command;

/// `cargo run -p xtask -- stage-helper --target <triple>`
///
/// `--target` is required rather than inferred. Every release build
/// passes an explicit `--target` to `tauri build` (the Intel macOS
/// bundle is cross-compiled on an Apple Silicon runner), so guessing
/// the host triple would silently stage the wrong binary for exactly
/// the build that is hardest to notice it in.
pub fn run(args: Vec<String>) -> Result<(), String> {
    let target = parse_target(&args)?;
    let root = workspace_root()?;

    let status = Command::new(cargo())
        .current_dir(&root)
        .args([
            "build",
            "--release",
            "--locked",
            "-p",
            "freally-helper",
            "--bin",
            "freally-helper",
            "--target",
            &target,
        ])
        .status()
        .map_err(|e| format!("could not run cargo build: {e}"))?;
    if !status.success() {
        return Err(format!("cargo build for {target} failed"));
    }

    let exe_suffix = if target.contains("windows") {
        ".exe"
    } else {
        ""
    };
    let built = target_dir(&root)
        .join(&target)
        .join("release")
        .join(format!("freally-helper{exe_suffix}"));
    if !built.is_file() {
        return Err(format!(
            "helper binary missing after a successful build: {}",
            built.display()
        ));
    }

    let dest_dir = root
        .join("apps")
        .join("freally-ui")
        .join("src-tauri")
        .join("binaries");
    std::fs::create_dir_all(&dest_dir)
        .map_err(|e| format!("could not create {}: {e}", dest_dir.display()))?;
    let dest = dest_dir.join(format!("freally-helper-{target}{exe_suffix}"));
    std::fs::copy(&built, &dest)
        .map_err(|e| format!("could not copy helper to {}: {e}", dest.display()))?;

    let bytes = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    println!(
        "staged {} ({bytes} bytes) for bundling as a sidecar",
        dest.display()
    );
    Ok(())
}

/// `--target <triple>` or `--target=<triple>`, and nothing else.
///
/// Deliberately not a loop: every branch returns on the first
/// argument, so looping would only obscure that this takes exactly one
/// option.
fn parse_target(args: &[String]) -> Result<String, String> {
    let mut it = args.iter();
    let Some(first) = it.next() else {
        return Err("missing --target <rust-target-triple>".to_string());
    };
    if let Some(v) = first.strip_prefix("--target=") {
        return non_empty(v);
    }
    if first == "--target" {
        return non_empty(it.next().map(String::as_str).unwrap_or_default());
    }
    Err(format!("unknown argument `{first}`"))
}

fn non_empty(v: &str) -> Result<String, String> {
    if v.trim().is_empty() {
        Err("--target needs a value".to_string())
    } else {
        Ok(v.trim().to_string())
    }
}

fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

/// Where cargo actually writes build output.
///
/// Not always `<root>/target`: `CARGO_TARGET_DIR` relocates it, and
/// the Linux parity container sets it to `/target` so the build cache
/// lives in a volume rather than on the bind-mounted source. Assuming
/// the default made staging fail there with "helper binary missing
/// after a successful build" — the build had succeeded, we were just
/// looking in the wrong place. A relative value is resolved against
/// the workspace root, which is where cargo resolves it from too.
fn target_dir(root: &Path) -> PathBuf {
    resolve_target_dir(root, std::env::var_os("CARGO_TARGET_DIR"))
}

/// The path half of [`target_dir`], with the environment passed in.
///
/// Separated so it can be tested directly. Setting `CARGO_TARGET_DIR`
/// from a test would need an `unsafe` block — the workspace denies
/// those — and would leak into whatever else reads it concurrently.
fn resolve_target_dir(root: &Path, configured: Option<std::ffi::OsString>) -> PathBuf {
    match configured {
        Some(v) if !v.is_empty() => {
            let p = PathBuf::from(v);
            if p.is_absolute() { p } else { root.join(p) }
        }
        _ => root.join("target"),
    }
}

/// The workspace root, from this crate's manifest directory.
fn workspace_root() -> Result<PathBuf, String> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .map_err(|_| "CARGO_MANIFEST_DIR is not set".to_string())?;
    Path::new(&manifest)
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "xtask manifest has no parent".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn target_is_parsed_in_both_spellings() {
        assert_eq!(
            parse_target(&v(&["--target", "x86_64-pc-windows-msvc"])).unwrap(),
            "x86_64-pc-windows-msvc"
        );
        assert_eq!(
            parse_target(&v(&["--target=aarch64-apple-darwin"])).unwrap(),
            "aarch64-apple-darwin"
        );
    }

    /// `CARGO_TARGET_DIR` moves the build output. The Linux parity
    /// container sets it to an absolute path outside the workspace, and
    /// assuming `<root>/target` made staging fail there even though the
    /// build had succeeded.
    #[test]
    fn target_dir_follows_cargo_target_dir() {
        let root = Path::new("/work");
        let os = |s: &str| Some(std::ffi::OsString::from(s));

        assert_eq!(
            resolve_target_dir(root, None),
            root.join("target"),
            "default layout"
        );
        assert_eq!(
            resolve_target_dir(root, os("")),
            root.join("target"),
            "an empty override is not an override"
        );
        assert_eq!(
            resolve_target_dir(root, os("/target")),
            PathBuf::from("/target"),
            "an absolute override is used as-is"
        );
        assert_eq!(
            resolve_target_dir(root, os("build-out")),
            root.join("build-out"),
            "a relative override resolves against the workspace root"
        );
    }

    /// Guessing the host triple would stage the wrong binary for the
    /// cross-compiled Intel macOS bundle, which is the build least
    /// likely to have anyone notice.
    #[test]
    fn a_missing_target_is_an_error_not_a_guess() {
        assert!(parse_target(&[]).is_err());
        assert!(parse_target(&v(&["--target"])).is_err());
        assert!(parse_target(&v(&["--target="])).is_err());
        assert!(parse_target(&v(&["--wat", "x"])).is_err());
    }
}
