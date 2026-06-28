//! Phase 7 follow-up — in-process `IExplorerCommand::Invoke` coverage.
//!
//! The Phase 7 brief asked for an end-to-end test of the Windows
//! copy-verb handoff. `phase_07b_shellext.ps1` only LoadLibrary-probes
//! the four COM exports, and driving the live Explorer right-click menu
//! needs a desktop session (it stays a manual smoke). This closes the
//! automatable half: build a REAL `IShellItemArray` from a temp file via
//! the Shell APIs, run it through the production `collect_paths` (the
//! previously-untested `Invoke` -> paths link), and drive the genuine
//! `CopyCommand::Invoke` vtable through its null-array guard. Combined
//! with the existing `build_argv` tests (paths -> argv), the full Invoke
//! data path is covered without a live shell.
#![cfg(windows)]

use std::os::windows::ffi::OsStrExt;

use windows::Win32::Foundation::E_POINTER;
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize, IBindCtx,
};
use windows::Win32::UI::Shell::{
    IExplorerCommand, IShellItem, IShellItemArray, SHCreateItemFromParsingName,
    SHCreateShellItemArrayFromShellItem,
};
use windows::core::PCWSTR;
use windows_core::ComObject;

use copythat_shellext::com::{CopyCommand, MoveCommand, collect_paths};

fn wide(path: &std::path::Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[test]
fn collect_paths_extracts_filesystem_path_from_a_real_shell_item_array() {
    // COM must be live on the calling thread for the Shell item APIs;
    // cargo runs each test on its own thread. S_FALSE (already inited)
    // is fine — we only need it live.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("evidence.bin");
    std::fs::write(&file, b"copy-that").unwrap();
    let wpath = wide(&file);

    // SAFETY: `wpath` is a live NUL-terminated UTF-16 buffer; the COM
    // objects are owned here and dropped at scope end.
    let paths = unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(wpath.as_ptr()), None).unwrap();
        let array: IShellItemArray = SHCreateShellItemArrayFromShellItem(&item).unwrap();
        collect_paths(&array).unwrap()
    };

    assert_eq!(paths.len(), 1, "expected exactly one filesystem path");
    // Compare via canonicalize: a CI runner hands back the long-form path
    // (e.g. `runneradmin`) while tempfile's path can carry an 8.3 short
    // component (`RUNNER~1`) — and casing can differ too. Canonicalizing
    // both sides resolves short names + case so the check is host-stable.
    let got = std::fs::canonicalize(std::path::Path::new(&paths[0])).unwrap();
    let want = std::fs::canonicalize(&file).unwrap();
    assert_eq!(
        got, want,
        "extracted path should resolve to the source file"
    );

    unsafe { CoUninitialize() };
}

#[test]
fn commands_construct_as_iexplorercommand_and_invoke_guards_null_array() {
    // The exact objects DllGetClassObject hands Explorer must be
    // constructible as IExplorerCommand — both verbs.
    let copy: IExplorerCommand = ComObject::new(CopyCommand).to_interface();
    let _move: IExplorerCommand = ComObject::new(MoveCommand).to_interface();

    // Driving the real vtable Invoke with a NULL array must hit the
    // E_POINTER guard in `invoke_with_verb` rather than spawn — proving
    // the vtable is reachable end-to-end without a live shell.
    // SAFETY: IExplorerCommand::Invoke is an unsafe COM call; null for
    // both the item array and the bind context is the documented case.
    let result = unsafe { copy.Invoke(None::<&IShellItemArray>, None::<&IBindCtx>) };
    let err = result.expect_err("null IShellItemArray must be rejected");
    assert_eq!(err.code(), E_POINTER);
}
