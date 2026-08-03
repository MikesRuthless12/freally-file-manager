// Installed-app detection (Phase 3 / FC-20..FC-22).
//
// The Rust backend (src-tauri/src/detect.rs) reads the version THIS machine has
// installed for each catalog app, from the installer's own record. The UI
// compares it to the latest published release to badge each card and label its
// primary action. Charter invariant: a version is shown only when an installer
// actually wrote it — never fabricated.

// One app's detected reading, mirroring the Rust `DetectResult`.
export interface DetectResult {
  id: string;
  /** The installed version, or null when the app isn't on this machine. */
  installedVersion: string | null;
}

// The install status of one catalog app on this machine, derived by comparing
// the detected version to the latest release.
export type InstallStatus = "not-installed" | "installed" | "update-available";

// What the card's download button offers, given the status (FC-22). Launching an
// installed app ("Open") arrives with the install flow in Phase 5 (FC-42); until
// then an up-to-date install offers "Re-download" of the verified installer.
export type DownloadAction = "install" | "update" | "redownload";
