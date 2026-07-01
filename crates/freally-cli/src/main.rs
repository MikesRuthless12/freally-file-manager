//! `freally` — Phase 36 CLI entry point.
//!
//! Parses argv via clap, dispatches to the chosen subcommand, and
//! propagates the documented exit code (see [`freally_cli::ExitCode`]).
//! All logic lives in the library crate so smoke tests can exercise
//! it end-to-end with `assert_cmd`.

fn main() -> std::process::ExitCode {
    freally_cli::run_from_argv()
}
