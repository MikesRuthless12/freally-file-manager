//! FFM-M24 — user-level launch at login.
//!
//! `Settings::general::start_with_os` has been a persisted preference
//! with nothing behind it; this module is the implementation. Three
//! per-user mechanisms, none of which needs elevation:
//!
//! - **Windows** — a value under
//!   `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
//! - **macOS** — a `RunAtLoad` LaunchAgent plist under
//!   `~/Library/LaunchAgents/`.
//! - **Linux** — a `.desktop` file under `$XDG_CONFIG_HOME/autostart`.
//!
//! The registered command always carries the app's own
//! `--start-minimized` flag: launch-at-login exists so the tray icon,
//! shell hooks, paste hotkey, clipboard watcher, and watchers are live
//! from login, not so a window appears in the user's face every boot.
//!
//! ## Portable installs
//!
//! Every mechanism above writes an **absolute path into host-global
//! state**. On a portable install that path points at a removable
//! volume, so after the stick is unplugged the host is left with a
//! login item that fails silently forever. Callers gate on
//! `freally_settings::portable::allows_os_integration()`; this module
//! additionally refuses a program path it cannot resolve.
//!
//! Rendering is separated from registration so the exact `.desktop`
//! and plist bytes are unit-testable on every host.

use std::path::{Path, PathBuf};

/// Registry value / plist label / desktop-file stem. Stable — changing
/// it orphans the entry an older build registered.
pub const AUTOSTART_ID: &str = "FreallyFileManager";

/// Flag appended to the registered command line.
pub const START_MINIMIZED_FLAG: &str = "--start-minimized";

/// Why enabling or disabling launch-at-login failed.
#[derive(Debug, thiserror::Error)]
pub enum AutostartError {
    /// The executable path could not be resolved or is not absolute.
    #[error("could not resolve an absolute path to this executable")]
    NoProgramPath,
    /// `$HOME` is unset, so the per-user location cannot be resolved.
    #[error("could not locate the user's home directory")]
    NoHomeDir,
    /// This target has no supported per-user autostart mechanism.
    #[error("launch at login is not supported on this platform")]
    Unsupported,
    /// Reading or writing the entry failed.
    #[error("autostart I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

/// The command line to register: this executable plus
/// [`START_MINIMIZED_FLAG`].
fn program() -> Result<PathBuf, AutostartError> {
    let exe = std::env::current_exe().map_err(|_| AutostartError::NoProgramPath)?;
    if !exe.is_absolute() {
        return Err(AutostartError::NoProgramPath);
    }
    Ok(exe)
}

/// Render the Linux `.desktop` autostart entry.
///
/// `Exec=` takes a quoted program path: a space in the install
/// directory would otherwise split into two arguments and the entry
/// would launch nothing.
pub fn render_desktop_entry(program: &Path, app_name: &str) -> String {
    let quoted = format!("\"{}\"", program.to_string_lossy().replace('"', "\\\""));
    format!(
        "[Desktop Entry]\nType=Application\nName={app_name}\nExec={quoted} {START_MINIMIZED_FLAG}\nTerminal=false\nX-GNOME-Autostart-enabled=true\nComment=Start {app_name} minimized to the tray at login\n"
    )
}

/// Render the macOS LaunchAgent plist for launch-at-login.
///
/// `RunAtLoad` (not `StartCalendarInterval`) — this fires once, at
/// login, which is the whole contract.
pub fn render_launch_agent(program: &Path) -> String {
    let escaped = xml_escape(&program.to_string_lossy());
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \x20 <key>Label</key>\n  <string>dev.freally.autostart</string>\n\
         \x20 <key>ProgramArguments</key>\n  <array>\n    <string>{escaped}</string>\n    <string>{START_MINIMIZED_FLAG}</string>\n  </array>\n\
         \x20 <key>RunAtLoad</key>\n  <true/>\n\
         \x20 <key>KeepAlive</key>\n  <false/>\n\
         </dict>\n\
         </plist>\n"
    )
}

/// Render the Windows `Run`-key value: a quoted program path plus the
/// flag. Windows parses this value as a command line, so the quoting
/// is what keeps `C:\Program Files\…` from splitting at the space.
pub fn render_run_key_value(program: &Path) -> String {
    format!(
        "\"{}\" {START_MINIMIZED_FLAG}",
        program.to_string_lossy().replace('"', "\"\"")
    )
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\'' => out.push_str("&apos;"),
            '"' => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(unix)]
fn home_dir() -> Result<PathBuf, AutostartError> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .ok_or(AutostartError::NoHomeDir)
}

#[cfg(target_os = "macos")]
fn agent_path() -> Result<PathBuf, AutostartError> {
    Ok(home_dir()?
        .join("Library")
        .join("LaunchAgents")
        .join("dev.freally.autostart.plist"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn desktop_path() -> Result<PathBuf, AutostartError> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(x) if !x.is_empty() => PathBuf::from(x),
        _ => home_dir()?.join(".config"),
    };
    Ok(base
        .join("autostart")
        .join(format!("{AUTOSTART_ID}.desktop")))
}

/// Register (`enabled = true`) or deregister the login item.
///
/// Idempotent in both directions: enabling twice rewrites the same
/// entry, and disabling something that was never registered succeeds.
pub fn set_enabled(enabled: bool, app_name: &str) -> Result<(), AutostartError> {
    #[cfg(target_os = "windows")]
    {
        let _ = app_name;
        windows_impl::set_enabled(enabled)
    }
    #[cfg(target_os = "macos")]
    {
        let _ = app_name;
        let path = agent_path()?;
        if enabled {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, render_launch_agent(&program()?))?;
        } else if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let path = desktop_path()?;
        if enabled {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, render_desktop_entry(&program()?, app_name))?;
        } else if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = (enabled, app_name);
        Err(AutostartError::Unsupported)
    }
}

/// Whether a login item is currently registered.
///
/// Reports what the **OS** holds, not what settings say, so a Settings
/// screen can show the truth after an entry was removed by hand (or by
/// a cleanup tool) behind the app's back.
pub fn is_enabled() -> Result<bool, AutostartError> {
    #[cfg(target_os = "windows")]
    {
        windows_impl::is_enabled()
    }
    #[cfg(target_os = "macos")]
    {
        Ok(agent_path()?.exists())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Ok(desktop_path()?.exists())
    }
    #[cfg(not(any(windows, unix)))]
    {
        Ok(false)
    }
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use super::{AUTOSTART_ID, AutostartError, program, render_run_key_value};

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    /// `reg.exe` rather than a direct `RegSetValueExW`: the registry
    /// write is a once-per-toggle operation, and shelling out keeps
    /// this module free of an `unsafe` block whose only job is to
    /// duplicate what a shipped OS tool already does correctly.
    fn reg(args: &[&str]) -> Result<std::process::Output, AutostartError> {
        // Absolute path — see `scheduler::os_tool`. A `reg.exe` beside
        // a portable install would otherwise win over System32's.
        Ok(std::process::Command::new(crate::scheduler::os_tool_pub("reg"))
            .args(args)
            .output()?)
    }

    pub fn set_enabled(enabled: bool) -> Result<(), AutostartError> {
        let key = format!(r"HKCU\{RUN_KEY}");
        if enabled {
            let value = render_run_key_value(&program()?);
            let out = reg(&[
                "ADD",
                &key,
                "/v",
                AUTOSTART_ID,
                "/t",
                "REG_SZ",
                "/d",
                &value,
                "/f",
            ])?;
            if !out.status.success() {
                return Err(AutostartError::Io(std::io::Error::other(
                    String::from_utf8_lossy(&out.stderr).trim().to_string(),
                )));
            }
        } else {
            // A missing value makes `reg DELETE` exit non-zero; that is
            // the already-disabled case, not a failure.
            let _ = reg(&["DELETE", &key, "/v", AUTOSTART_ID, "/f"])?;
        }
        Ok(())
    }

    pub fn is_enabled() -> Result<bool, AutostartError> {
        let key = format!(r"HKCU\{RUN_KEY}");
        let out = reg(&["QUERY", &key, "/v", AUTOSTART_ID])?;
        Ok(out.status.success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_desktop_entry_quotes_the_program_and_starts_minimized() {
        let entry = render_desktop_entry(
            Path::new("/opt/Freally File Manager/freally-ui"),
            "Freally File Manager",
        );
        assert!(
            entry.contains("Exec=\"/opt/Freally File Manager/freally-ui\" --start-minimized"),
            "{entry}"
        );
        assert!(entry.contains("Type=Application"));
        assert!(entry.contains("Name=Freally File Manager"));
        assert!(entry.contains("Terminal=false"));
    }

    #[test]
    fn the_launch_agent_runs_at_load_and_escapes_the_path() {
        let plist = render_launch_agent(Path::new("/Applications/Freally & Co.app/freally"));
        assert!(plist.contains("<key>RunAtLoad</key>\n  <true/>"), "{plist}");
        assert!(plist.contains("<string>/Applications/Freally &amp; Co.app/freally</string>"));
        assert!(plist.contains("<string>--start-minimized</string>"));
        // KeepAlive must stay false: this is a login item, not a daemon
        // launchd should resurrect every time the user quits the app.
        assert!(plist.contains("<key>KeepAlive</key>\n  <false/>"));
    }

    #[test]
    fn the_run_key_value_quotes_a_program_files_path() {
        let value = render_run_key_value(Path::new(r"C:\Program Files\Freally\freally-ui.exe"));
        assert_eq!(
            value,
            r#""C:\Program Files\Freally\freally-ui.exe" --start-minimized"#
        );
    }

    #[test]
    fn every_rendered_form_carries_the_minimized_flag() {
        let p = Path::new("/x/freally");
        assert!(render_desktop_entry(p, "Freally").contains(START_MINIMIZED_FLAG));
        assert!(render_launch_agent(p).contains(START_MINIMIZED_FLAG));
        assert!(render_run_key_value(p).contains(START_MINIMIZED_FLAG));
    }
}
