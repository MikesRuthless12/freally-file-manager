// Download engine types (Phase 4 / FC-30..32). The engine itself is native
// (src-tauri/src/download.rs); the UI drives it over an IPC channel and renders
// real byte progress — every percent shown is bytes-received / published-size.

import type { InstallerAsset } from "../releases/types";

/// What the UI hands the engine for one download (mirrors Rust's DownloadRequest).
export interface DownloadRequest {
  id: string; // catalog app id — also the cancel key
  url: string;
  fileName: string;
  expectedSize: number;
  expectedSha256: string | null; // the API's "sha256:<hex>" digest, when published
}

/// Events streamed back from the engine (mirrors Rust's DownloadEvent).
export type DownloadEvent =
  | { kind: "started"; total: number; resumedFrom: number }
  | { kind: "progress"; received: number; total: number }
  | { kind: "verifying" }
  | { kind: "done"; path: string; sha256: string; checksumVerified: boolean }
  | { kind: "failed"; code: string; detail: string }
  | { kind: "canceled" };

/// What the UI hands the engine to install one app (mirrors Rust's
/// InstallRequest). Only the id — the engine resolves it to a file through its
/// own verified-downloads registry; the UI can never name a file to execute.
export interface InstallRequest {
  id: string;
}

/// Per-app install events streamed back from the engine (mirrors Rust's
/// InstallEvent). Every requested id gets exactly one terminal event.
export type InstallEvent =
  | { kind: "started"; id: string }
  | { kind: "progress"; id: string; fraction: number }
  | { kind: "installed"; id: string }
  | { kind: "failed"; id: string; code: string; detail: string }
  | { kind: "canceled"; id: string };

/// The engine boundary. Production is the Tauri backend; tests inject a scripted
/// fake (see engine.ts) so flows are drivable outside the shell.
export interface DownloadEngine {
  /** Resolves once the download reaches a terminal state (done/failed/canceled). */
  start(request: DownloadRequest, onEvent: (event: DownloadEvent) => void): Promise<void>;
  /** Cancel by app id; the engine answers with a terminal "canceled" event. */
  cancel(id: string): void;
  /** Silently install a batch of verified downloads, sequentially (FC-40).
   *  Resolves once every requested id has received its terminal event. */
  install(requests: InstallRequest[], onEvent: (event: InstallEvent) => void): Promise<void>;
  /** Cancel the running install batch: pending installs end "canceled"; a
   *  running installer finishes and reports its real outcome. */
  cancelInstalls(): void;
}

/// The engine's stable failure codes (mirrors Rust's `fail` module), plus the
/// UI-side "unknown" for anything unrecognized. Typing the closed set keeps
/// every code-keyed lookup (messages, cancel affordances) compiler-checked.
export const DOWNLOAD_FAIL_CODES = [
  "badRequest",
  "alreadyDownloading",
  "network",
  "sizeMismatch",
  "shortDownload",
  "checksumMismatch",
  "io",
  "unknown",
] as const;
export type DownloadFailCode = (typeof DOWNLOAD_FAIL_CODES)[number];

/// The install engine's stable failure codes (mirrors Rust's install `fail`
/// module), plus the UI-side "unknown". The policy refusals (noChecksum /
/// untrustedSource / tampered) are the Phase 5 trust gate saying no — the UI
/// must render them as refusals, never as success.
export const INSTALL_FAIL_CODES = [
  "notDownloaded",
  "noChecksum",
  "untrustedSource",
  "tampered",
  "unsupportedInstaller",
  "installerFailed",
  "elevationDeclined",
  "busy",
  "io",
  "unknown",
] as const;
export type InstallFailCode = (typeof INSTALL_FAIL_CODES)[number];

/// The finished download that "done" and every install phase carry: the file
/// is on disk and the panels keep offering it (Show in folder, retry Install)
/// whatever the install outcome.
export interface DownloadedFile {
  path: string;
  checksumVerified: boolean;
}

/// One app's download-and-install as the UI tracks it. "verifying" keeps the
/// byte counts so the bar holds at 100% while the checksum runs; "done" records
/// what was actually verified (honest: checksum only when a digest was
/// published). Failed/canceled keep the real bytes received so aggregate
/// progress stays monotonic — a terminal entry freezes at what it transferred.
export type AppDownloadState =
  | { phase: "downloading"; received: number; total: number }
  | { phase: "verifying"; received: number; total: number }
  | ({ phase: "done" } & DownloadedFile)
  | { phase: "failed"; code: DownloadFailCode; received: number }
  | { phase: "canceled"; received: number }
  | ({ phase: "waitingInstall" } & DownloadedFile)
  | ({ phase: "installing"; fraction: number } & DownloadedFile)
  | ({ phase: "installed" } & DownloadedFile)
  | ({ phase: "installFailed"; code: InstallFailCode } & DownloadedFile)
  | ({ phase: "installCanceled" } & DownloadedFile);

/// An entry in the Download All queue: the app id plus its resolved installer.
export interface BatchEntry {
  appId: string;
  asset: InstallerAsset;
}
