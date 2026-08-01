//! Command-line interface for shell integration.
//!
//! Parses the argv handed to the app binary by shell-extension hosts
//! (Phase 7b Windows COM DLL, Phase 7c macOS Finder Sync Extension,
//! the Phase 7a Linux `.desktop` / ServiceMenu / UCA files). Two
//! entry paths:
//!
//! - No CLI args → normal GUI launch.
//! - `--enqueue <verb> <paths…> [--destination <dst>]` → route the
//!   paths into the job queue. When the app is already running, the
//!   `tauri-plugin-single-instance` plugin forwards argv to the live
//!   instance and this process exits; when it isn't, we parse on
//!   first boot and dispatch from `.setup()`.
//!
//! The CLI intentionally stays minimal and stable: shell extensions
//! are host-of-record for the argv, and changing the flag names means
//! re-shipping those extensions. New flags append, never replace.

use std::ffi::OsString;
use std::path::PathBuf;

/// What the app should do on launch, as derived from argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliAction {
    /// Launch the GUI with no pre-queued work.
    Run,
    /// Dispatch a shell-integration enqueue request.
    Enqueue(EnqueueArgs),
    /// FFM-M09 — open the hash inspector over these paths. Deliberately
    /// not an `--enqueue` verb: hashing produces no job, so it must not
    /// travel the copy/move queue path.
    Hash(Vec<PathBuf>),
    /// Print help and exit.
    PrintHelp,
    /// Print version and exit.
    PrintVersion,
}

/// Which shell verb was invoked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueVerb {
    Copy,
    Move,
}

impl EnqueueVerb {
    /// Stable wire name — mirrors `ipc::job_kind_name` so the
    /// frontend can branch on the same strings it already uses for
    /// `JobKind`.
    pub fn as_str(self) -> &'static str {
        match self {
            EnqueueVerb::Copy => "copy",
            EnqueueVerb::Move => "move",
        }
    }
}

/// Parsed `--enqueue` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnqueueArgs {
    pub verb: EnqueueVerb,
    pub paths: Vec<PathBuf>,
    /// Optional. When present, the job runs non-interactively and
    /// skips the drop-staging dialog — this is the scripted-use path.
    /// When absent, the app emits a `shell-enqueue` event and the
    /// frontend reuses its drop-staging flow to pick a destination.
    pub destination: Option<PathBuf>,
    /// FFM-M13 — read the source set from a TXT / CSV / JSON manifest
    /// instead of (or in addition to) the argv paths.
    pub files_from: Option<PathBuf>,
    /// FFM-M13 — preserve each listed file's path relative to this
    /// root under the destination, instead of flattening to basenames.
    pub relative_to: Option<PathBuf>,
}

/// Errors encountered while parsing CLI arguments. User-facing
/// messages land on stderr; they are intentionally not localised
/// (CLI output is a scripting / developer surface, not UI).
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CliError {
    #[error("--enqueue requires a verb (copy or move)")]
    MissingVerb,
    #[error("unknown enqueue verb: {0}")]
    UnknownVerb(String),
    #[error("at least one path is required")]
    NoPaths,
    #[error("unknown argument: {0}")]
    Unknown(String),
    #[error("--destination requires a path")]
    MissingDestination,
    #[error("--files-from requires a path")]
    MissingFilesFrom,
    #[error("--relative-to requires a path")]
    MissingRelativeTo,
}

/// Short CLI help text printed by `--help` / `-h`. Not localised; see
/// [`CliError`] note above.
pub const HELP: &str = "\
Freally File Manager v0.22.0 — shell-integration CLI

Usage:
    freally                                  Launch the GUI
    freally --enqueue copy <paths…>          Queue a copy job per path
    freally --enqueue move <paths…>          Queue a move job per path
                                              (optionally: --destination <dst>)
    freally --enqueue copy --files-from <manifest> --destination <dst>
                                              Queue every path listed in a
                                              TXT / CSV / JSON manifest
                                              (optionally: --relative-to <root>
                                              to preserve the tree structure)
    freally --hash <paths…>                  Open the hash inspector over the paths
    freally --portable                       Keep all settings/history/journal
                                              beside the binary (FFM-M21)
    freally --start-minimized                Launch straight to the tray (FFM-M24)
    freally --help | -h
    freally --version | -V
";

/// Current crate version. Reported by `--version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Parse argv into a [`CliAction`].
///
/// `argv` is the full process argv including `argv[0]`; the first
/// element is always skipped. Non-UTF-8 arguments are accepted (we
/// round-trip via `PathBuf`) but flag names must be valid UTF-8.
pub fn parse_args(argv: Vec<OsString>) -> Result<CliAction, CliError> {
    let mut iter = argv.into_iter();
    // Skip argv[0] — the binary path.
    let _ = iter.next();

    let mut enqueue: Option<EnqueueArgs> = None;
    let mut hash_paths: Option<Vec<PathBuf>> = None;
    let mut help = false;
    let mut version = false;

    while let Some(raw) = iter.next() {
        let Some(flag) = raw.to_str() else {
            // A non-UTF-8 argument outside of path position is an
            // error — all flag names are ASCII.
            return Err(CliError::Unknown(raw.to_string_lossy().into_owned()));
        };
        match flag {
            "--help" | "-h" => help = true,
            "--version" | "-V" => version = true,
            // FFM-M21 / FFM-M24 — launch-mode flags. Both are read
            // before `parse_args` runs (see [`portable_requested`] and
            // [`start_minimized_requested`]), because path resolution
            // and window creation happen earlier than argv dispatch.
            // They are accepted-and-ignored here so they don't fall
            // through to `CliError::Unknown`.
            "--portable" | "--start-minimized" => {}
            "--enqueue" => {
                let verb_raw = iter.next().ok_or(CliError::MissingVerb)?;
                let verb = match verb_raw.to_str() {
                    Some("copy") => EnqueueVerb::Copy,
                    Some("move") => EnqueueVerb::Move,
                    Some(other) => return Err(CliError::UnknownVerb(other.to_string())),
                    None => {
                        return Err(CliError::UnknownVerb(
                            verb_raw.to_string_lossy().into_owned(),
                        ));
                    }
                };
                let mut flags = EnqueueFlags::default();
                let paths = collect_paths(&mut iter, Some(&mut flags))?;
                // A manifest supplies the source set on its own, so
                // argv paths are only required without `--files-from`.
                if paths.is_empty() && flags.files_from.is_none() {
                    return Err(CliError::NoPaths);
                }
                enqueue = Some(EnqueueArgs {
                    verb,
                    paths,
                    destination: flags.destination,
                    files_from: flags.files_from,
                    relative_to: flags.relative_to,
                });
            }
            // FFM-M09 — hash inspector. Takes paths only; the enqueue
            // flags are meaningless here and are rejected as unknown.
            "--hash" => {
                let paths = collect_paths(&mut iter, None)?;
                if paths.is_empty() {
                    return Err(CliError::NoPaths);
                }
                hash_paths = Some(paths);
            }
            other => return Err(CliError::Unknown(other.to_string())),
        }
    }

    if help {
        return Ok(CliAction::PrintHelp);
    }
    if version {
        return Ok(CliAction::PrintVersion);
    }
    if let Some(eq) = enqueue {
        return Ok(CliAction::Enqueue(eq));
    }
    if let Some(paths) = hash_paths {
        return Ok(CliAction::Hash(paths));
    }
    Ok(CliAction::Run)
}

/// FFM-M21 — whether argv asks for portable mode.
///
/// Read before anything resolves a path: `freally_settings::portable`
/// caches its answer on first use, so the environment variable it reads
/// has to be set before the first `Settings::default_path()` call, which
/// happens long before `parse_args` dispatches a `CliAction`.
pub fn portable_requested(argv: &[OsString]) -> bool {
    argv.iter().any(|a| a.to_str() == Some("--portable"))
}

/// FFM-M24 — whether argv asks for a minimized (tray-only) launch.
///
/// The registered login item always carries this flag: launch-at-login
/// exists so the tray, shell hooks, hotkey, watcher, and schedules are
/// live from login, not so a window appears every boot.
pub fn start_minimized_requested(argv: &[OsString]) -> bool {
    argv.iter().any(|a| a.to_str() == Some("--start-minimized"))
}

/// The flag slots only the `--enqueue` form accepts.
#[derive(Debug, Default)]
struct EnqueueFlags {
    destination: Option<PathBuf>,
    files_from: Option<PathBuf>,
    relative_to: Option<PathBuf>,
}

/// Consume the rest of argv as a path list.
///
/// `--` terminates flag parsing so a file literally named `--help.txt`
/// still round-trips; any other `--flag` is an error. When `flags` is
/// `Some`, the `--enqueue`-only companion flags are accepted and write
/// through it; when it is `None` they are rejected like any other
/// unknown flag. Emptiness is the caller's rule to enforce — a
/// `--files-from` enqueue legitimately has no argv paths.
fn collect_paths(
    iter: &mut impl Iterator<Item = OsString>,
    mut flags: Option<&mut EnqueueFlags>,
) -> Result<Vec<PathBuf>, CliError> {
    let mut paths: Vec<PathBuf> = Vec::new();
    while let Some(next) = iter.next() {
        // Only inspect strings when it *looks* like a flag; otherwise
        // treat as a path to preserve non-UTF-8 filenames byte-for-byte.
        if let Some(s) = next.to_str() {
            match s {
                "--destination" | "-d" if flags.is_some() => {
                    let dst = iter.next().ok_or(CliError::MissingDestination)?;
                    if let Some(slot) = flags.as_deref_mut() {
                        slot.destination = Some(PathBuf::from(dst));
                    }
                    continue;
                }
                // FFM-M13 — source set from a manifest, optionally
                // laid out relative to a root instead of flattened.
                "--files-from" if flags.is_some() => {
                    let manifest = iter.next().ok_or(CliError::MissingFilesFrom)?;
                    if let Some(slot) = flags.as_deref_mut() {
                        slot.files_from = Some(PathBuf::from(manifest));
                    }
                    continue;
                }
                "--relative-to" if flags.is_some() => {
                    let root = iter.next().ok_or(CliError::MissingRelativeTo)?;
                    if let Some(slot) = flags.as_deref_mut() {
                        slot.relative_to = Some(PathBuf::from(root));
                    }
                    continue;
                }
                "--" => {
                    for rest in iter.by_ref() {
                        paths.push(PathBuf::from(rest));
                    }
                    break;
                }
                other if other.starts_with("--") => {
                    return Err(CliError::Unknown(other.to_string()));
                }
                _ => {}
            }
        }
        paths.push(PathBuf::from(next));
    }
    Ok(paths)
}

/// Convenience for tests and for `std::env::args_os()` at runtime.
pub fn parse_args_iter<I, S>(argv: I) -> Result<CliAction, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    parse_args(argv.into_iter().map(Into::into).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<OsString> {
        v.iter().map(|s| OsString::from(*s)).collect()
    }

    #[test]
    fn bare_launch_returns_run() {
        assert_eq!(parse_args(args(&["freally"])).unwrap(), CliAction::Run);
    }

    #[test]
    fn help_flag() {
        assert_eq!(
            parse_args(args(&["freally", "--help"])).unwrap(),
            CliAction::PrintHelp
        );
        assert_eq!(
            parse_args(args(&["freally", "-h"])).unwrap(),
            CliAction::PrintHelp
        );
    }

    #[test]
    fn version_flag() {
        assert_eq!(
            parse_args(args(&["freally", "--version"])).unwrap(),
            CliAction::PrintVersion
        );
    }

    #[test]
    fn enqueue_copy_single_path() {
        let a = parse_args(args(&["freally", "--enqueue", "copy", "/src/a"])).unwrap();
        assert_eq!(
            a,
            CliAction::Enqueue(EnqueueArgs {
                verb: EnqueueVerb::Copy,
                paths: vec![PathBuf::from("/src/a")],
                destination: None,
                files_from: None,
                relative_to: None,
            })
        );
    }

    #[test]
    fn enqueue_move_multiple_paths() {
        let a = parse_args(args(&["freally", "--enqueue", "move", "/a", "/b", "/c"])).unwrap();
        let CliAction::Enqueue(eq) = a else {
            panic!("expected Enqueue");
        };
        assert_eq!(eq.verb, EnqueueVerb::Move);
        assert_eq!(eq.paths.len(), 3);
        assert_eq!(eq.paths[0], PathBuf::from("/a"));
        assert_eq!(eq.paths[2], PathBuf::from("/c"));
    }

    #[test]
    fn enqueue_with_destination_flag() {
        let a = parse_args(args(&[
            "freally",
            "--enqueue",
            "copy",
            "/a",
            "--destination",
            "/dst",
            "/b",
        ]))
        .unwrap();
        let CliAction::Enqueue(eq) = a else {
            panic!();
        };
        assert_eq!(eq.paths, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(eq.destination, Some(PathBuf::from("/dst")));
    }

    #[test]
    fn enqueue_double_dash_terminates_flags() {
        // After `--`, everything is a path even if it starts with --.
        let a = parse_args(args(&[
            "freally",
            "--enqueue",
            "copy",
            "--",
            "--weird",
            "/b",
        ]))
        .unwrap();
        let CliAction::Enqueue(eq) = a else {
            panic!();
        };
        assert_eq!(
            eq.paths,
            vec![PathBuf::from("--weird"), PathBuf::from("/b")]
        );
    }

    #[test]
    fn missing_verb_errors() {
        assert_eq!(
            parse_args(args(&["freally", "--enqueue"])).unwrap_err(),
            CliError::MissingVerb
        );
    }

    #[test]
    fn unknown_verb_errors() {
        assert_eq!(
            parse_args(args(&["freally", "--enqueue", "nuke", "/a"])).unwrap_err(),
            CliError::UnknownVerb("nuke".to_string())
        );
    }

    #[test]
    fn no_paths_errors() {
        // `--` on its own with no paths after — but we swallow the --
        // before checking emptiness.
        assert_eq!(
            parse_args(args(&["freally", "--enqueue", "copy"])).unwrap_err(),
            CliError::NoPaths
        );
    }

    #[test]
    fn missing_destination_value_errors() {
        assert_eq!(
            parse_args(args(&[
                "freally",
                "--enqueue",
                "copy",
                "/a",
                "--destination"
            ]))
            .unwrap_err(),
            CliError::MissingDestination
        );
    }

    #[test]
    fn unknown_top_level_flag_errors() {
        assert_eq!(
            parse_args(args(&["freally", "--nope"])).unwrap_err(),
            CliError::Unknown("--nope".to_string())
        );
    }

    #[test]
    fn verb_str_round_trips() {
        assert_eq!(EnqueueVerb::Copy.as_str(), "copy");
        assert_eq!(EnqueueVerb::Move.as_str(), "move");
    }

    // FFM-M09 — `--hash` opens the inspector instead of queueing work.

    #[test]
    fn hash_flag_collects_paths() {
        assert_eq!(
            parse_args(args(&["freally", "--hash", "/a", "/b"])).unwrap(),
            CliAction::Hash(vec![PathBuf::from("/a"), PathBuf::from("/b")])
        );
    }

    #[test]
    fn hash_flag_honours_the_double_dash_terminator() {
        assert_eq!(
            parse_args(args(&["freally", "--hash", "--", "--weird.txt"])).unwrap(),
            CliAction::Hash(vec![PathBuf::from("--weird.txt")])
        );
    }

    #[test]
    fn hash_flag_requires_a_path() {
        assert_eq!(
            parse_args(args(&["freally", "--hash"])).unwrap_err(),
            CliError::NoPaths
        );
    }

    // FFM-M13 — `--files-from` / `--relative-to`.

    #[test]
    fn files_from_supplies_the_source_set_without_argv_paths() {
        let a = parse_args(args(&[
            "freally",
            "--enqueue",
            "copy",
            "--files-from",
            "/lists/failed.txt",
            "--destination",
            "/dst",
        ]))
        .unwrap();
        let CliAction::Enqueue(eq) = a else {
            panic!("expected Enqueue");
        };
        assert!(eq.paths.is_empty());
        assert_eq!(eq.files_from, Some(PathBuf::from("/lists/failed.txt")));
        assert_eq!(eq.destination, Some(PathBuf::from("/dst")));
        assert_eq!(eq.relative_to, None);
    }

    #[test]
    fn files_from_composes_with_argv_paths_and_relative_to() {
        let a = parse_args(args(&[
            "freally",
            "--enqueue",
            "copy",
            "/extra/one.txt",
            "--files-from",
            "/lists/l.csv",
            "--relative-to",
            "/root",
        ]))
        .unwrap();
        let CliAction::Enqueue(eq) = a else {
            panic!("expected Enqueue");
        };
        assert_eq!(eq.paths, vec![PathBuf::from("/extra/one.txt")]);
        assert_eq!(eq.files_from, Some(PathBuf::from("/lists/l.csv")));
        assert_eq!(eq.relative_to, Some(PathBuf::from("/root")));
    }

    #[test]
    fn files_from_and_relative_to_require_a_value() {
        assert_eq!(
            parse_args(args(&["freally", "--enqueue", "copy", "--files-from"])).unwrap_err(),
            CliError::MissingFilesFrom
        );
        assert_eq!(
            parse_args(args(&[
                "freally",
                "--enqueue",
                "copy",
                "/a",
                "--relative-to"
            ]))
            .unwrap_err(),
            CliError::MissingRelativeTo
        );
    }

    #[test]
    fn enqueue_without_paths_or_a_manifest_still_errors() {
        assert_eq!(
            parse_args(args(&[
                "freally",
                "--enqueue",
                "copy",
                "--destination",
                "/dst"
            ]))
            .unwrap_err(),
            CliError::NoPaths
        );
    }

    #[test]
    fn hash_flag_rejects_the_enqueue_only_flags() {
        // `--files-from` makes no sense for a hash run and must not
        // silently swallow the next argument.
        assert_eq!(
            parse_args(args(&["freally", "--hash", "/a", "--files-from", "/l.txt"])).unwrap_err(),
            CliError::Unknown("--files-from".to_string())
        );
    }

    #[test]
    fn hash_flag_rejects_destination() {
        // Hashing writes nothing, so `--destination` is not a valid
        // companion flag — it must not silently swallow the next arg.
        assert_eq!(
            parse_args(args(&["freally", "--hash", "/a", "--destination", "/d"])).unwrap_err(),
            CliError::Unknown("--destination".to_string())
        );
    }
}
