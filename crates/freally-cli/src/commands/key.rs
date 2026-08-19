//! `freally key list|add|remove|recovery` — Phase 50i's CLI half.
//!
//! The multi-slot keyfile has been in `freally-chunk` since 50i; this
//! exposes it so a headless host can add a device credential or mint a
//! recovery key without the desktop UI.
//!
//! **This is an access gate, not at-rest encryption** (scrypt,
//! `log_n = 15`). Removing a slot revokes that credential; it does not
//! re-encrypt anything, because nothing is encrypted with it. Phase 51
//! owns real envelope crypto.
//!
//! Passphrases come from `--auth` / `--password`, falling back to
//! `FREALLY_REPO_PASSWORD` / `FREALLY_NEW_PASSWORD`. Prefer the env
//! vars: an argument is visible to any local process that can read
//! `/proc/<pid>/cmdline` or run `ps`.

use std::sync::Arc;

use crate::ExitCode;
use crate::cli::{GlobalArgs, KeyArgs, KeyOp};
use crate::output::{JsonEventKind, OutputMode, OutputWriter};

use super::{fail, open_repo};

fn resolve(explicit: Option<String>, env_key: &str) -> Option<String> {
    explicit.or_else(|| std::env::var(env_key).ok().filter(|v| !v.is_empty()))
}

pub(crate) async fn run(
    _global: &GlobalArgs,
    args: KeyArgs,
    writer: Arc<OutputWriter>,
) -> ExitCode {
    let repo = match open_repo() {
        Ok(r) => r,
        Err(message) => return fail(&writer, message),
    };

    match args.op {
        KeyOp::List => match repo.list_keys() {
            Ok(slots) if slots.is_empty() => {
                let _ = writer.human("no key slots — this repository has no passphrase gate");
                ExitCode::Success
            }
            Ok(slots) => {
                for s in &slots {
                    // `human` is a no-op outside Human mode, so a
                    // JSON caller would otherwise get an empty stdout
                    // and be unable to tell "no slots" from "three".
                    let _ = writer.emit(JsonEventKind::Info {
                        message: format!("{}\t{}", s.kind, s.label),
                    });
                    let _ = writer.human(&format!("{}\t{}", s.kind, s.label));
                }
                ExitCode::Success
            }
            Err(e) => fail(&writer, format!("list keys: {e}")),
        },

        KeyOp::Add {
            label,
            password,
            auth,
        } => {
            let Some(password) = resolve(password, "FREALLY_NEW_PASSWORD") else {
                return fail(
                    &writer,
                    "a new-slot password is required (--password or FREALLY_NEW_PASSWORD)"
                        .to_string(),
                );
            };
            let auth = resolve(auth, "FREALLY_REPO_PASSWORD");
            match repo.add_key(auth.as_deref(), &password, &label) {
                Ok(()) => {
                    let _ = writer.human(&format!("added key slot `{label}`"));
                    ExitCode::Success
                }
                Err(e) => fail(&writer, format!("add key: {e}")),
            }
        }

        KeyOp::Remove { label } => match repo.remove_key(&label) {
            Ok(true) => {
                let _ = writer.human(&format!("removed key slot `{label}`"));
                ExitCode::Success
            }
            // Nothing was revoked. An offboarding script chaining on
            // `&&` must not read a typo'd label as a completed
            // revocation, so this is a failure, not a no-op.
            Ok(false) => fail(&writer, format!("no key slot named `{label}`")),
            // The last-slot refusal lands here: removing it would leave
            // the repository permanently unopenable.
            Err(e) => fail(&writer, format!("remove key: {e}")),
        },

        KeyOp::Recovery { auth } => {
            // Minting replaces any previous recovery slot and only the
            // verifier is stored, so the returned string is the only
            // copy that will ever exist. `human` is a no-op in Quiet
            // mode, which would destroy the old key and discard the new
            // one with nothing on stdout — refuse before touching the
            // keyfile rather than after.
            if writer.mode() == OutputMode::Quiet {
                return fail(
                    &writer,
                    "`key recovery` prints a one-time secret and cannot run under --quiet"
                        .to_string(),
                );
            }
            let auth = resolve(auth, "FREALLY_REPO_PASSWORD");
            match repo.generate_recovery_key(auth.as_deref()) {
                Ok(secret) => {
                    // Printed once and never stored — only its verifier
                    // is written. There is no second chance to read it,
                    // so it goes out on both channels.
                    let _ = writer.emit(JsonEventKind::Info {
                        message: format!("recovery key: {secret}"),
                    });
                    let _ = writer.human("recovery key (shown once — store it now):");
                    let _ = writer.human(&secret);
                    ExitCode::Success
                }
                Err(e) => fail(&writer, format!("generate recovery key: {e}")),
            }
        }
    }
}
