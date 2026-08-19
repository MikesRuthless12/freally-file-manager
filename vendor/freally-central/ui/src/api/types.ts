// Mirrors the Rust structs returned by the Tauri backend commands
// (src-tauri/src/commands.rs). Everything derivable from the build comes from
// the build, so these panels can never claim a version the binary isn't.
export interface BuildInfo {
  version: string;
  authors: string;
  projectStarted: string;
  firstStableReleased: string | null;
  os: string;
  arch: string;
  homepage: string;
  repository: string;
  issues: string;
  copyright: string;
}

// The running build's changelog section, for Settings → What's New.
export interface ReleaseNotes {
  version: string;
  notes: string | null;
}
