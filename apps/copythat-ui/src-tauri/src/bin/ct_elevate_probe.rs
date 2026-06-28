//! Phase 17d — manual UAC-probe (dev-only; gated behind `--features
//! elevate-probe`, never built in normal/app/release builds).
//!
//! Calls the REAL `copythat_ui_lib::elevate::elevated_retry`, so the
//! genuine OS consent prompt fires — UAC `Start-Process -Verb RunAs`
//! on Windows, `pkexec` on Linux, `osascript … with administrator
//! privileges` on macOS — and the elevated `copythat-helper` performs
//! the copy. This is the one piece of the 17d path that can't be
//! auto-tested (a human clicks the consent dialog).
//!
//! Usage:
//!   ct_elevate_probe [<src> <dst>]
//! With no args it writes a temp source file and copies it to a temp
//! destination. Pass an explicit <dst> in a location your unprivileged
//! user can't write to prove the copy really ran elevated. See
//! docs/MANUAL-UAC-TEST.md.

use std::path::PathBuf;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (src, dst) = match args.as_slice() {
        [s, d] => (PathBuf::from(s), PathBuf::from(d)),
        [] => {
            let dir = std::env::temp_dir();
            let pid = std::process::id();
            let src = dir.join(format!("ct-uac-probe-src-{pid}.bin"));
            let dst = dir.join(format!("ct-uac-probe-dst-{pid}.bin"));
            std::fs::write(&src, b"phase-17d UAC probe payload\n").expect("write temp src");
            let _ = std::fs::remove_file(&dst);
            eprintln!(
                "[probe] no args; using temp src={} -> dst={}",
                src.display(),
                dst.display()
            );
            (src, dst)
        }
        _ => {
            eprintln!("usage: ct_elevate_probe [<src> <dst>]");
            std::process::exit(2);
        }
    };

    eprintln!(
        "[probe] calling elevated_retry — the OS consent prompt \
         (UAC / pkexec / osascript) should appear NOW."
    );
    match copythat_ui_lib::elevate::elevated_retry(&src, &dst).await {
        Ok(resp) => {
            eprintln!("[probe] OK — helper Response: {resp:?}");
            eprintln!(
                "[probe] ElevatedRetryOk {{ bytes }} means the elevated helper \
                 copied the file; check that {} now exists.",
                dst.display()
            );
        }
        Err(e) => {
            eprintln!(
                "[probe] elevation failed/cancelled (the UI shows \
                 retry-elevated-unavailable here): {e}"
            );
            std::process::exit(1);
        }
    }
}
