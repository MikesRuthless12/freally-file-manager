//! FFM-M21 — portable mode.
//!
//! A portable install keeps everything it writes **beside the binary**
//! instead of under the OS config/data directories, so a USB stick
//! carries the whole app: settings, profiles, history, journal, audit
//! log, plugins, and the chunk-store repository.
//!
//! ## How it is detected
//!
//! Two signals, checked once and then cached for the process lifetime
//! (paths must not change underneath a running app):
//!
//! 1. The `FREALLY_PORTABLE` environment variable set to anything other
//!    than `0` / `false` / empty. The app sets this itself when
//!    launched with `--portable`, before it resolves any path.
//! 2. A `freally-portable.txt` marker file next to the executable —
//!    the zip-distributed build ships one. A marker makes the mode
//!    survive being launched by double-click, where no flag is passed.
//!
//! ## What changes
//!
//! - [`config_root`] and [`data_root`] both resolve to
//!   `<exe-dir>/FreallyData`, so a single directory is the entire
//!   user-state footprint.
//! - Shell registration and launch-at-login are **refused**
//!   ([`allows_os_integration`]): both write absolute paths into the
//!   host's registry / LaunchAgents / autostart directory, which would
//!   outlive the stick and then point at nothing.
//! ## What portable mode does *not* do yet
//!
//! Two honest limitations, stated here because the UI copy is written
//! against this list:
//!
//! - **The non-portable directory identity is still split.** Every
//!   writer now checks [`portable_root`] before falling back to its
//!   own resolver, so a portable install keeps `settings.toml`, the
//!   profile store, history DB, resume journal, audit log, chunk
//!   store, scan index and per-scan DBs, thumbnail cache, drop-stack,
//!   and plugin dir beside the binary. What is *not* unified is the
//!   fallback each writer uses when the install is normal: the
//!   engine-side crates resolve `com.Freally.freally-file-manager`
//!   while this crate resolves `dev.freally.freally-file-manager`.
//!   Those are the same directory on Windows and Linux, but not on
//!   macOS — so routing the fallbacks through [`data_root`] would
//!   orphan an existing macOS install's data. Unifying them is a
//!   migration, not a rename.
//! - **A forgotten credential passphrase is unrecoverable.** Cloud
//!   secrets no longer reach the host keystore: under portable mode
//!   `freally_cloud::Credentials` uses an age-encrypted file on the
//!   drive, unlocked by a passphrase the user chooses and held in
//!   memory for the process lifetime only. Nothing on the stick can
//!   decrypt it — that is the point, and it means there is no reset
//!   path. `settings-portable-keychain-warning` says so; do not soften
//!   *that* claim either.
//!
//! Nothing here creates directories — resolvers stay side-effect free
//! so a probe (`is_portable()` in a settings screen) can't litter a
//! `FreallyData` folder next to the binary of a normal install.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::error::{Result, SettingsError};

/// Marker filename that opts a directory-local install into portable
/// mode.
pub const PORTABLE_MARKER: &str = "freally-portable.txt";

/// Environment variable the app sets for itself when `--portable` is
/// passed. Also honoured when set externally, which is what makes a
/// portable launcher script possible.
pub const PORTABLE_ENV: &str = "FREALLY_PORTABLE";

/// Directory name created beside the binary to hold all user state.
pub const PORTABLE_DIRNAME: &str = "FreallyData";

static PORTABLE: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Interpret an environment-variable value as a boolean opt-in.
/// Absent, empty, `0`, `false`, and `no` are all "not portable"; every
/// other value opts in.
fn env_opts_in(raw: Option<&str>) -> bool {
    match raw.map(str::trim) {
        None | Some("") => false,
        Some(v) => !matches!(
            v.to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
    }
}

/// Resolve the portable root for a given executable path, if portable
/// mode applies. Pure — takes the two inputs rather than reading the
/// process environment, so the decision matrix is unit-testable.
fn resolve_root(exe_dir: Option<&Path>, env: Option<&str>) -> Option<PathBuf> {
    let dir = exe_dir?;
    if env_opts_in(env) || dir.join(PORTABLE_MARKER).is_file() {
        return Some(dir.join(PORTABLE_DIRNAME));
    }
    None
}

fn detect(cli_requested: bool) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    let exe_dir = exe.as_deref().and_then(Path::parent).map(Path::to_path_buf);
    let env = std::env::var(PORTABLE_ENV).ok();
    let signal = if cli_requested {
        Some("1".to_string())
    } else {
        env
    };
    resolve_root(exe_dir.as_deref(), signal.as_deref())
}

/// Settle portable mode at startup, folding in a `--portable` flag.
///
/// Call once, before anything resolves a path. Returns whether this
/// process is portable. Later calls are no-ops that return the
/// already-settled answer — deciding twice is exactly the bug this
/// guards against, since the app would read settings from one root and
/// write them to another.
///
/// Exists so the app does not have to mutate its own environment: a
/// `set_var` at startup is a data race by definition once any thread
/// exists, and this crate would rather not ship an `unsafe` block for
/// a boolean.
pub fn init(cli_requested: bool) -> bool {
    PORTABLE.get_or_init(|| detect(cli_requested)).is_some()
}

/// The portable root for this process, or `None` for a normal install.
///
/// Evaluated once. A later environment change does not move it.
pub fn portable_root() -> Option<&'static Path> {
    PORTABLE.get_or_init(|| detect(false)).as_deref()
}

/// Whether this process is running portable.
pub fn is_portable() -> bool {
    portable_root().is_some()
}

/// Where configuration lives: the portable root, or the OS config dir.
pub fn config_root() -> Result<PathBuf> {
    if let Some(root) = portable_root() {
        return Ok(root.to_path_buf());
    }
    Ok(crate::project_dirs()?.config_dir().to_path_buf())
}

/// Where bulk data lives (history DB, journal, chunk store): the
/// portable root, or the OS data dir.
pub fn data_root() -> Result<PathBuf> {
    if let Some(root) = portable_root() {
        return Ok(root.to_path_buf());
    }
    Ok(crate::project_dirs()?.data_dir().to_path_buf())
}

/// Whether OS integration (shell context menu, copy interception,
/// launch-at-login) may be registered.
///
/// `false` under portable mode: every one of those writes an absolute
/// path into host-global state that outlives the removable volume the
/// binary is on.
pub fn allows_os_integration() -> bool {
    !is_portable()
}

/// Whether the OS keychain is an *appropriate* place for stored
/// credentials on this install.
///
/// `false` under portable mode: a keychain entry stays on the host
/// after the stick is unplugged, which is both a leak and a credential
/// the portable install cannot reach on the next machine.
///
/// Enforced, not merely reported: `cloud_commands::credential_store`
/// selects `Credentials::Keychain` for a normal install and
/// `Credentials::EncryptedFile` under portable mode, so a portable
/// install never opens a host keychain entry. The Settings banner
/// still reads this to explain which store is in use.
pub fn allows_os_keychain() -> bool {
    !is_portable()
}

/// Make `path` relative to the portable root when it sits inside it.
///
/// Bookmarks (FFM-M20) and pinned destinations are stored relative in
/// portable mode so they survive the stick mounting as `E:` on one
/// machine and `/media/usb` on the next. Paths outside the root — a
/// bookmark pointing at the host's own disk — are returned unchanged,
/// because they were never portable to begin with.
pub fn relativize(path: &Path) -> PathBuf {
    match portable_root() {
        Some(root) => match path.strip_prefix(root) {
            Ok(rel) => rel.to_path_buf(),
            Err(_) => path.to_path_buf(),
        },
        None => path.to_path_buf(),
    }
}

/// Inverse of [`relativize`]: resolve a stored bookmark back to an
/// absolute path.
///
/// A relative path is resolved against the portable root; an absolute
/// one is returned as-is. Outside portable mode a relative path has no
/// root to resolve against, so it is returned unchanged and fails
/// later at the normal path-validation gate rather than silently
/// resolving against the process working directory.
pub fn absolutize(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match portable_root() {
        Some(root) => root.join(path),
        None => path.to_path_buf(),
    }
}

/// Create the portable root if it does not exist. Called once at
/// startup by a portable launch, never by a probe.
pub fn ensure_root() -> Result<()> {
    if let Some(root) = portable_root() {
        std::fs::create_dir_all(root).map_err(|e| SettingsError::Write {
            path: root.to_path_buf(),
            source: e,
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_values_that_opt_in_and_out() {
        for on in ["1", "true", "yes", "TRUE", "portable", " 1 "] {
            assert!(env_opts_in(Some(on)), "{on:?} should opt in");
        }
        for off in ["", "0", "false", "FALSE", "no", "off", "  "] {
            assert!(!env_opts_in(Some(off)), "{off:?} should not opt in");
        }
        assert!(!env_opts_in(None));
    }

    #[test]
    fn the_env_var_alone_selects_the_portable_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = resolve_root(Some(dir.path()), Some("1")).expect("portable");
        assert_eq!(root, dir.path().join(PORTABLE_DIRNAME));
    }

    #[test]
    fn a_marker_file_alone_selects_the_portable_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(PORTABLE_MARKER), b"").unwrap();
        let root = resolve_root(Some(dir.path()), None).expect("portable");
        assert_eq!(root, dir.path().join(PORTABLE_DIRNAME));
    }

    #[test]
    fn a_marker_directory_is_not_a_marker() {
        // `is_file()` rather than `exists()`: a stray directory of that
        // name must not silently relocate every path the app writes.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(PORTABLE_MARKER)).unwrap();
        assert!(resolve_root(Some(dir.path()), None).is_none());
    }

    #[test]
    fn no_signal_means_a_normal_install() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_root(Some(dir.path()), None).is_none());
        assert!(resolve_root(Some(dir.path()), Some("0")).is_none());
    }

    #[test]
    fn an_unknown_exe_dir_can_never_be_portable() {
        assert!(resolve_root(None, Some("1")).is_none());
    }

    #[test]
    fn relativize_and_absolutize_are_inverses_outside_portable_mode() {
        // The test process is not portable, so both are identity — the
        // property that keeps a normal install's absolute bookmarks
        // untouched.
        let p = if cfg!(windows) {
            PathBuf::from(r"C:\Users\me\Pictures")
        } else {
            PathBuf::from("/home/me/Pictures")
        };
        assert_eq!(relativize(&p), p);
        assert_eq!(absolutize(&p), p);
    }

    #[test]
    fn absolutize_leaves_absolute_paths_alone() {
        let p = if cfg!(windows) {
            PathBuf::from(r"D:\data")
        } else {
            PathBuf::from("/data")
        };
        assert_eq!(absolutize(&p), p);
    }
}
