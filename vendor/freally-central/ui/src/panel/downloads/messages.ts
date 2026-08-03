// Map the engine's failure codes onto localized message keys — exhaustive over
// the typed code set, so adding a code without a message is a compile error.
// Verify failures get their own honest message (the file was refused and
// discarded); transport-ish failures share the network message.

import type { DownloadFailCode, InstallFailCode } from "./types";

const FAILURE_KEY: Record<DownloadFailCode, string> = {
  network: "fcp-dl-failed-network",
  shortDownload: "fcp-dl-failed-network",
  sizeMismatch: "fcp-dl-failed-verify",
  checksumMismatch: "fcp-dl-failed-verify",
  alreadyDownloading: "fcp-dl-failed-busy",
  badRequest: "fcp-dl-failed",
  io: "fcp-dl-failed",
  unknown: "fcp-dl-failed",
};

export function failureMessageKey(code: DownloadFailCode): string {
  return FAILURE_KEY[code];
}

// Install failures (Phase 5). The trust-gate refusals get their own honest
// messages — "the file was refused" is a different fact from "the installer
// errored", and the DoD forbids blurring either into success.
const INSTALL_FAILURE_KEY: Record<InstallFailCode, string> = {
  notDownloaded: "fcp-install-failed-not-downloaded",
  noChecksum: "fcp-install-failed-not-verified",
  untrustedSource: "fcp-install-failed-not-verified",
  tampered: "fcp-install-failed-verify",
  unsupportedInstaller: "fcp-install-failed-unsupported",
  installerFailed: "fcp-install-failed-installer",
  elevationDeclined: "fcp-install-failed-elevation",
  busy: "fcp-install-failed-busy",
  io: "fcp-install-failed",
  unknown: "fcp-install-failed",
};

export function installFailureMessageKey(code: InstallFailCode): string {
  return INSTALL_FAILURE_KEY[code];
}
