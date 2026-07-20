//! Registration helpers for the shell extension.
//!
//! The pure-Rust parts (registry key-path composition, value strings,
//! install-scope toggle) are unit-tested on every host; the actual
//! `RegCreateKeyExW` / `RegDeleteTreeW` calls are Windows-only.
//!
//! Three groups of keys are managed:
//!
//! 1. **Class registration** — `…\Software\Classes\CLSID\{guid}` and
//!    its `InprocServer32` subkey, pointing the COM runtime at the
//!    DLL file.
//! 2. **Verb registration** — `…\Software\Classes\*\shell\<verb>` and
//!    `…\Software\Classes\Directory\shell\<verb>` so both files and
//!    folders pick up the entry. `DelegateExecute` targets the CLSID
//!    so Explorer hands `Invoke` to our `IExplorerCommand`.
//! 3. **Copy-verb interceptor** — optional opt-in toggle under
//!    `HKCU\Software\Classes\*\shell\copy\DelegateExecute` that
//!    makes the app intercept Explorer's default Ctrl-C / drag-copy
//!    flow. Off by default; Phase 12 exposes the toggle in Settings.

use crate::consts::{CLSID_COPY_STR, CLSID_MOVE_STR, VERB_COPY, VERB_MOVE};

/// Where to write the registration keys.
///
/// `PerUser` → `HKCU\Software\Classes\…` — does not require admin.
/// `LocalMachine` → `HKLM\Software\Classes\…` — requires admin; used
/// by the MSI installer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallScope {
    PerUser,
    LocalMachine,
}

impl InstallScope {
    /// Top-level hive name as it appears in registry-key paths.
    pub fn hive(self) -> &'static str {
        match self {
            InstallScope::PerUser => "HKCU",
            InstallScope::LocalMachine => "HKLM",
        }
    }
}

/// Compose the full registry path for a class-identifier (CLSID) key.
///
/// Example: `clsid_path(PerUser, CLSID_COPY_STR)` →
/// `HKCU\Software\Classes\CLSID\{A7D2C001-C097-4C96-8F7A-5C970C097001}`
pub fn clsid_path(scope: InstallScope, clsid: &str) -> String {
    format!(r"{}\Software\Classes\CLSID\{}", scope.hive(), clsid)
}

/// Compose the `…\CLSID\{guid}\InprocServer32` key path. This is the
/// value Windows reads to find our DLL file.
pub fn inproc_server_path(scope: InstallScope, clsid: &str) -> String {
    format!(r"{}\InprocServer32", clsid_path(scope, clsid))
}

/// Build the verb-key path (`…\Classes\<target>\shell\<verb>`) for a
/// given shell target (`"*"` for all files, `"Directory"` for folders).
pub fn verb_path(scope: InstallScope, target: &str, verb: &str) -> String {
    format!(
        r"{}\Software\Classes\{}\shell\{}",
        scope.hive(),
        target,
        verb
    )
}

/// The two targets a right-click on a selection can produce:
/// a loose file / group of files, and a folder or group of folders.
/// We register against both so the menu is uniform.
pub const SHELL_TARGETS: [&str; 2] = ["*", "Directory"];

/// The one-stop list of `(hive, subpath, value, is_default)` tuples
/// needed to register one command. Provided for the install script
/// and host-tested without touching the actual registry.
///
/// Return format is `(key_path, name_or_empty, value)` — an empty
/// name string means "(Default)". This maps cleanly onto
/// `RegSetValueExW(...)` calls in [`apply_registration`] below.
pub fn class_registration_keys(
    scope: InstallScope,
    clsid: &str,
    display: &str,
    dll_path: &str,
) -> Vec<(String, String, String)> {
    vec![
        // Class root — friendly display name goes in the (Default) value.
        (clsid_path(scope, clsid), String::new(), display.to_string()),
        // InprocServer32 — path to the DLL.
        (
            inproc_server_path(scope, clsid),
            String::new(),
            dll_path.to_string(),
        ),
        // Threading model — Apartment is the safe default for shell
        // extensions that touch UI-thread-owned data (Explorer is STA).
        (
            inproc_server_path(scope, clsid),
            "ThreadingModel".to_string(),
            "Apartment".to_string(),
        ),
    ]
}

/// Per-target verb-key tuples. One invocation produces entries for
/// `*` (loose files) and `Directory` (folders), each with its
/// display caption + `DelegateExecute` pointing back at the CLSID.
pub fn verb_registration_keys(
    scope: InstallScope,
    verb: &str,
    clsid: &str,
    display: &str,
) -> Vec<(String, String, String)> {
    let mut out = Vec::with_capacity(SHELL_TARGETS.len() * 3);
    for target in SHELL_TARGETS {
        let base = verb_path(scope, target, verb);
        out.push((base.clone(), String::new(), display.to_string()));
        // `MUIVerb` is the modern display value Explorer prefers; we
        // write both so a shell that ignores one still picks up the
        // other.
        out.push((base.clone(), "MUIVerb".to_string(), display.to_string()));
        out.push((base, "DelegateExecute".to_string(), clsid.to_string()));
    }
    out
}

/// The single HKCU key whose `DelegateExecute` value reroutes
/// Explorer's built-in copy verb. Reverting the interceptor deletes
/// exactly this key, restoring the OS handler (the built-in verb lives
/// in HKLM/HKCR and is never touched).
pub const COPY_INTERCEPTOR_KEY: &str = r"HKCU\Software\Classes\*\shell\copy";

/// Copy-verb interceptor — opt-in TeraCopy-style override.
///
/// Writes `HKCU\Software\Classes\*\shell\copy\DelegateExecute={clsid}`
/// so Explorer routes its default copy verb (Ctrl-C, drag-copy, the
/// "Copy" entry at the top of the context menu) to our command.
/// System-wide install is intentionally not supported — the
/// interceptor is a per-user preference.
pub fn copy_interceptor_keys(clsid: &str) -> Vec<(String, String, String)> {
    vec![(
        COPY_INTERCEPTOR_KEY.to_string(),
        "DelegateExecute".to_string(),
        clsid.to_string(),
    )]
}

/// All keys needed to register the full extension (both verbs, both
/// targets, both CLSIDs). Call sites feed the output directly into
/// [`apply_registration`] / [`delete_registration`].
pub fn all_registration_keys(scope: InstallScope, dll_path: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    out.extend(class_registration_keys(
        scope,
        CLSID_COPY_STR,
        "Freally File Manager v0.19.85 — Copy command",
        dll_path,
    ));
    out.extend(class_registration_keys(
        scope,
        CLSID_MOVE_STR,
        "Freally File Manager v0.19.85 — Move command",
        dll_path,
    ));
    out.extend(verb_registration_keys(
        scope,
        VERB_COPY,
        CLSID_COPY_STR,
        "Copy with Freally File Manager",
    ));
    out.extend(verb_registration_keys(
        scope,
        VERB_MOVE,
        CLSID_MOVE_STR,
        "Move with Freally File Manager",
    ));
    out
}

// ---------------------------------------------------------------------
// Windows-only registry I/O.
// ---------------------------------------------------------------------

#[cfg(windows)]
mod windows_impl {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
        RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW,
    };
    use windows::core::{PCWSTR, Result as WinResult};

    fn to_wide(s: &str) -> Vec<u16> {
        OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Split a path like `HKCU\Software\Classes\foo` into the root
    /// predefined key and the remainder.
    fn split_hive(path: &str) -> Option<(HKEY, &str)> {
        if let Some(rest) = path.strip_prefix(r"HKCU\") {
            Some((HKEY_CURRENT_USER, rest))
        } else if let Some(rest) = path.strip_prefix(r"HKLM\") {
            Some((HKEY_LOCAL_MACHINE, rest))
        } else {
            None
        }
    }

    /// Create a key (and any intermediate keys) and write one value.
    ///
    /// `value_name` empty means "(Default)".
    pub fn write_value(key_path: &str, value_name: &str, data: &str) -> WinResult<()> {
        let Some((hive, subpath)) = split_hive(key_path) else {
            return Err(windows::core::Error::from_hresult(
                windows::Win32::Foundation::E_INVALIDARG,
            ));
        };
        let wide_sub = to_wide(subpath);
        let mut key: HKEY = HKEY::default();
        // SAFETY: all pointers we hand to RegCreateKeyExW outlive the
        // call; wide_sub is null-terminated; the output `key` is
        // zero-initialised and Windows fills it.
        unsafe {
            RegCreateKeyExW(
                hive,
                PCWSTR(wide_sub.as_ptr()),
                Some(0),
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut key,
                None,
            )
            .ok()?;
        }

        let result = (|| -> WinResult<()> {
            let wide_name_storage;
            let name_ptr: PCWSTR = if value_name.is_empty() {
                PCWSTR::null()
            } else {
                wide_name_storage = to_wide(value_name);
                PCWSTR(wide_name_storage.as_ptr())
            };
            // Intentionally use `to_wide` here too so the trailing
            // NUL is persisted — Explorer reads the value with
            // REG_SZ semantics and expects null termination.
            let wide_data = to_wide(data);
            let bytes = wide_data.as_ptr() as *const u8;
            let len = (wide_data.len() * std::mem::size_of::<u16>()) as u32;
            // SAFETY: name_ptr / bytes borrow stack-owned buffers that
            // outlive the syscall; RegSetValueExW is documented to
            // accept a nullable value name.
            unsafe {
                RegSetValueExW(
                    key,
                    name_ptr,
                    Some(0),
                    REG_SZ,
                    Some(std::slice::from_raw_parts(bytes, len as usize)),
                )
                .ok()?;
            }
            Ok(())
        })();

        // SAFETY: `key` was populated by the RegCreateKeyExW call above.
        unsafe {
            let _ = RegCloseKey(key);
        }
        result
    }

    /// Read a `REG_SZ` value, returning `None` if the key or value is
    /// absent. Used to probe whether the copy interceptor is active and
    /// whether the target CLSID is class-registered before we point the
    /// OS copy verb at it.
    pub fn read_sz(key_path: &str, value_name: &str) -> Option<String> {
        use windows::Win32::System::Registry::{RRF_RT_REG_SZ, RegGetValueW};
        let (hive, subpath) = split_hive(key_path)?;
        let wide_sub = to_wide(subpath);
        let wide_name = to_wide(value_name);
        // Query the size first (chars, incl. NUL), then read into a
        // right-sized buffer. `RegGetValueW` opens + queries + closes.
        let mut byte_len: u32 = 0;
        // SAFETY: wide_sub / wide_name are NUL-terminated and outlive
        // the call; a null data pointer with a live length pointer is
        // the documented size-probe form.
        let rc = unsafe {
            RegGetValueW(
                hive,
                PCWSTR(wide_sub.as_ptr()),
                PCWSTR(wide_name.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                None,
                Some(&mut byte_len),
            )
        };
        if rc.is_err() || byte_len == 0 {
            return None;
        }
        let mut buf = vec![0u16; byte_len as usize / 2];
        let mut have = byte_len;
        // SAFETY: buf is sized to the length the probe reported and
        // outlives the call; `have` is updated in place.
        let rc = unsafe {
            RegGetValueW(
                hive,
                PCWSTR(wide_sub.as_ptr()),
                PCWSTR(wide_name.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
                Some(&mut have),
            )
        };
        if rc.is_err() {
            return None;
        }
        // Trim the trailing NUL(s) the API includes in the char count.
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..end]))
    }

    /// Whether a copy/move CLSID is class-registered (its
    /// `InprocServer32` default value names a DLL) in either hive.
    /// Guards [`super::apply_copy_interceptor`]: pointing Explorer's
    /// copy verb at an unregistered CLSID would break Ctrl+C.
    pub fn clsid_registered(clsid: &str) -> bool {
        for scope in [
            super::InstallScope::PerUser,
            super::InstallScope::LocalMachine,
        ] {
            let path = super::inproc_server_path(scope, clsid);
            if read_sz(&path, "").is_some_and(|dll| !dll.is_empty()) {
                return true;
            }
        }
        false
    }

    /// Recursively delete a key and its subkeys. Missing keys are not
    /// an error — uninstall is idempotent.
    pub fn delete_tree(key_path: &str) -> WinResult<()> {
        let Some((hive, subpath)) = split_hive(key_path) else {
            return Err(windows::core::Error::from_hresult(
                windows::Win32::Foundation::E_INVALIDARG,
            ));
        };
        let wide = to_wide(subpath);
        // SAFETY: wide is null-terminated and outlives the call.
        let rc = unsafe { RegDeleteTreeW(hive, PCWSTR(wide.as_ptr())) };
        // Per docs: ERROR_FILE_NOT_FOUND (2) means the key wasn't
        // there — treat as success so we stay idempotent.
        if rc.0 == 0 || rc.0 == 2 {
            Ok(())
        } else {
            Err(windows::core::Error::from_hresult(
                windows::core::HRESULT::from_win32(rc.0),
            ))
        }
    }

    /// Write every key in `keys` to the registry. Bail on first
    /// failure; the partial state is intentional — the caller
    /// should call [`delete_registration`] to clean up.
    pub fn apply_registration(keys: &[(String, String, String)]) -> WinResult<()> {
        for (path, name, value) in keys {
            write_value(path, name, value)?;
        }
        Ok(())
    }

    /// Remove every key-prefix referenced by a registration set.
    pub fn delete_registration(keys: &[(String, String, String)]) -> WinResult<()> {
        // Delete unique key paths (a given path has multiple values;
        // `RegDeleteTreeW` nukes the whole key so we only need to
        // hit each path once).
        let mut seen = std::collections::BTreeSet::new();
        for (path, _, _) in keys {
            if seen.insert(path.clone()) {
                delete_tree(path)?;
            }
        }
        Ok(())
    }

    /// Turn on the copy-verb interceptor (the TeraCopy takeover).
    ///
    /// Refuses — with `E_FAIL` — unless the copy CLSID is already
    /// class-registered, because writing the `DelegateExecute` override
    /// while the handler DLL is absent would leave Explorer's Ctrl+C /
    /// paste pointing at nothing. The context-menu integration
    /// (`regsvr32` / the installer) registers that class; enabling
    /// interception without it is the one way this feature could harm a
    /// user, so it is blocked at the source.
    pub fn apply_copy_interceptor() -> WinResult<()> {
        use crate::consts::CLSID_COPY_STR;
        if !clsid_registered(CLSID_COPY_STR) {
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_FAIL,
                "the Freally copy handler is not registered; enable context-menu integration first",
            ));
        }
        apply_registration(&super::copy_interceptor_keys(CLSID_COPY_STR))
    }

    /// Revert to the OS copy handler by deleting the single HKCU
    /// override key. Idempotent (a missing key is success), and it
    /// never touches the built-in verb in HKLM/HKCR.
    pub fn remove_copy_interceptor() -> WinResult<()> {
        delete_tree(super::COPY_INTERCEPTOR_KEY)
    }

    /// Whether our interceptor is currently the active copy handler —
    /// i.e. the override key's `DelegateExecute` names the copy CLSID.
    pub fn copy_interceptor_active() -> bool {
        use crate::consts::CLSID_COPY_STR;
        read_sz(super::COPY_INTERCEPTOR_KEY, "DelegateExecute")
            .is_some_and(|v| v.eq_ignore_ascii_case(CLSID_COPY_STR))
    }
}

#[cfg(windows)]
pub use windows_impl::{
    apply_copy_interceptor, apply_registration, clsid_registered, copy_interceptor_active,
    delete_registration, delete_tree, read_sz, remove_copy_interceptor, write_value,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hive_maps_to_expected_prefix() {
        assert_eq!(InstallScope::PerUser.hive(), "HKCU");
        assert_eq!(InstallScope::LocalMachine.hive(), "HKLM");
    }

    #[test]
    fn clsid_path_builds_per_user_prefix() {
        let p = clsid_path(InstallScope::PerUser, CLSID_COPY_STR);
        assert_eq!(
            p,
            r"HKCU\Software\Classes\CLSID\{A7D2C001-C097-4C96-8F7A-5C970C097001}"
        );
    }

    #[test]
    fn clsid_path_builds_local_machine_prefix() {
        let p = clsid_path(InstallScope::LocalMachine, CLSID_MOVE_STR);
        assert!(p.starts_with(r"HKLM\Software\Classes\CLSID\"));
    }

    #[test]
    fn inproc_server_path_is_subkey_of_clsid_path() {
        let parent = clsid_path(InstallScope::PerUser, CLSID_COPY_STR);
        let child = inproc_server_path(InstallScope::PerUser, CLSID_COPY_STR);
        assert!(
            child.starts_with(&parent),
            "InprocServer32 must sit under CLSID: {child}"
        );
        assert!(child.ends_with(r"\InprocServer32"));
    }

    #[test]
    fn verb_path_targets_both_files_and_directories() {
        let p1 = verb_path(InstallScope::PerUser, "*", VERB_COPY);
        let p2 = verb_path(InstallScope::PerUser, "Directory", VERB_COPY);
        assert!(p1.contains(r"\*\shell\Freally.Copy"));
        assert!(p2.contains(r"\Directory\shell\Freally.Copy"));
    }

    #[test]
    fn class_registration_tuples_cover_default_and_threading_model() {
        let keys = class_registration_keys(
            InstallScope::PerUser,
            CLSID_COPY_STR,
            "Copy",
            r"C:\freally-shellext.dll",
        );
        // Exactly three tuples: class default, InprocServer32 default,
        // InprocServer32 ThreadingModel.
        assert_eq!(keys.len(), 3);
        assert!(keys.iter().any(|(_, name, _)| name == "ThreadingModel"));
        // The DLL path must appear verbatim as the InprocServer32 default.
        assert!(
            keys.iter()
                .any(|(_, name, value)| name.is_empty() && value == r"C:\freally-shellext.dll")
        );
    }

    #[test]
    fn verb_registration_tuples_target_all_files_and_folders() {
        let keys = verb_registration_keys(InstallScope::PerUser, VERB_COPY, CLSID_COPY_STR, "Copy");
        // 2 shell targets × 3 values (default + MUIVerb + DelegateExecute).
        assert_eq!(keys.len(), 6);
        assert!(keys.iter().any(|(p, _, _)| p.contains(r"\*\shell\")));
        assert!(
            keys.iter()
                .any(|(p, _, _)| p.contains(r"\Directory\shell\"))
        );
        // Every verb key must carry a `DelegateExecute` pointing at the CLSID.
        let delegate_count = keys
            .iter()
            .filter(|(_, name, value)| name == "DelegateExecute" && value == CLSID_COPY_STR)
            .count();
        assert_eq!(delegate_count, SHELL_TARGETS.len());
    }

    #[test]
    fn copy_interceptor_writes_single_hkcu_key() {
        let keys = copy_interceptor_keys(CLSID_COPY_STR);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].0, COPY_INTERCEPTOR_KEY);
        assert!(keys[0].0.starts_with(r"HKCU\Software\Classes\*\shell\copy"));
        assert_eq!(keys[0].1, "DelegateExecute");
        assert_eq!(keys[0].2, CLSID_COPY_STR);
    }

    #[test]
    fn interceptor_key_is_per_user_and_targets_the_copy_verb() {
        // Revert deletes exactly this key; it must be the HKCU override,
        // never the machine-wide built-in verb.
        assert!(COPY_INTERCEPTOR_KEY.starts_with(r"HKCU\"));
        assert!(COPY_INTERCEPTOR_KEY.ends_with(r"\*\shell\copy"));
    }

    #[test]
    fn all_registration_keys_cover_both_verbs() {
        let keys = all_registration_keys(InstallScope::PerUser, r"C:\freally-shellext.dll");
        // 2 classes × 3 tuples + 2 verbs × 6 tuples = 18.
        assert_eq!(keys.len(), 18);
        assert!(
            keys.iter()
                .any(|(_, _, v)| v == "Copy with Freally File Manager")
        );
        assert!(
            keys.iter()
                .any(|(_, _, v)| v == "Move with Freally File Manager")
        );
    }
}
