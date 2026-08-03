// Live release data resolved from the GitHub Releases API for a catalog app.
// The manifest supplies the repo + per-OS asset regexes (see catalog/types.ts);
// GitHub supplies the real version, date, installer assets, and download counts.
// Charter invariant: every number here is a real GitHub figure — never seeded.

import type { AppAssets } from "../catalog/types";

// The OS keys are exactly the manifest's per-OS asset slots, so derive them from
// AppAssets rather than restating the literals (a new platform tracks automatically).
export type OsKey = keyof AppAssets;

// The one canonical value-level list of those keys — every iteration site
// imports this so a new platform is added in exactly one place.
export const OS_KEYS: readonly OsKey[] = ["windows", "macos", "linux"];

// One downloadable installer asset from the release, as published by the
// GitHub API — the download engine (Phase 4) verifies against `size` and
// `digest`, so both are kept exactly as the API stated them.
export interface InstallerAsset {
  name: string; // asset file name, e.g. "Freally-Capture_1.4.0_x64-setup.exe"
  url: string; // browser_download_url
  size: number; // exact byte size per the API
  digest: string | null; // "sha256:<hex>" when GitHub published one
}

export interface ReleaseInfo {
  tag: string; // raw tag, e.g. "v1.2.3"
  version: string; // display version with the leading "v" stripped, e.g. "1.2.3"
  publishedAt: string; // ISO 8601 date from GitHub
  htmlUrl: string; // the release page on GitHub
  notes: string | null; // release body (markdown), shown verbatim in the viewer
  // Downloads of the matched INSTALLER assets per OS (summed across every asset
  // that matches the OS pattern — e.g. Linux .AppImage + .deb + .rpm). A key is
  // present only when the release ships at least one installer for that OS.
  perOs: Partial<Record<OsKey, number>>;
  // The matched installer assets themselves, per OS — what the Phase 4 download
  // engine streams and verifies. Same present-only convention as `perOs`.
  installers: Partial<Record<OsKey, InstallerAsset[]>>;
  // The app's real installer total = the sum of the per-OS installer downloads.
  // Non-installer assets (the updater's latest.json, .sig signatures, blockmaps)
  // are deliberately excluded so the figure reflects actual installs, and so the
  // Total always reconciles with the per-OS rows shown in the detail view.
  totalDownloads: number;
}

// The resolved state for one app:
//  - "available":   a release was found.
//  - "none":        the repo has no releases yet (coming-soon apps) — honest, not an error.
//  - "unavailable": the API was unreachable / rate-limited — counts are HIDDEN, never faked.
export type ReleaseState =
  | { status: "available"; release: ReleaseInfo }
  | { status: "none" }
  | { status: "unavailable" };

// The single place display sites unwrap the union: the live ReleaseInfo when a
// release is available, else null (also covering the not-yet-loaded undefined).
export function liveRelease(state: ReleaseState | undefined): ReleaseInfo | null {
  return state?.status === "available" ? state.release : null;
}
