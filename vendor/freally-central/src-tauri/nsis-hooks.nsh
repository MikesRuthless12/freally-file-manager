; Freally Central — NSIS installer hooks.
;
; Create a Desktop shortcut on install (and remove it on uninstall).
;
; Tauri's NSIS template creates only the Start-Menu entry, and its config schema
; has no desktop-shortcut option (verified on Windows 11 with tauri-cli 2.11.x:
; no Desktop icon after an NSIS install; same gap fixed for Freally Capture in
; commit f4d0738). Tauri's *MSI* (WiX) template already ships a Desktop shortcut
; by default (component `ApplicationShortcutDesktop`), so only NSIS needs this.
;
; No icon args: the shortcut inherits the target exe's own icon (the app icon
; from `bundle.icon`). $DESKTOP follows the installer's per-user/per-machine
; context, same as the Start-Menu entry. Updates re-create it by design — the
; installer's contract is "installed apps have their icon", stated here once.
!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "Creating the Desktop shortcut"
  CreateShortcut "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  Delete "$DESKTOP\${PRODUCTNAME}.lnk"
!macroend
