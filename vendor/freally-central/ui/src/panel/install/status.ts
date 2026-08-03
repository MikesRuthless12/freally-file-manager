// Derive an app's install status (FC-21) and its primary action (FC-22) from the
// detected installed version and the latest published release version.

import { compareVersions } from "./semver";
import type { DownloadAction, InstallStatus } from "./types";

/**
 * @param installedVersion the version detected on this machine, or null/undefined
 *   when the app isn't installed (or detection hasn't produced a reading).
 * @param latestVersion the latest release version, or null/undefined when the
 *   release feed is unknown (offline / rate-limited / coming-soon).
 *
 * Honest by construction: "update-available" is claimed only when BOTH versions
 * are known and the installed one is strictly older. An unknown latest never
 * downgrades an installed app to "update-available".
 */
export function computeStatus(
  installedVersion: string | null | undefined,
  latestVersion: string | null | undefined,
): InstallStatus {
  if (!installedVersion) return "not-installed";
  if (latestVersion && compareVersions(installedVersion, latestVersion) < 0) {
    return "update-available";
  }
  return "installed";
}

/**
 * The badge status for a card, or null when detection produced no reading for
 * this app — `installedVersion === undefined` means "not probed" (coming-soon
 * apps, or running outside the Tauri shell), which must NOT be badged, versus
 * `null` which means "probed and absent" → Not installed. Shared by the card and
 * detail view so both surfaces derive status identically.
 */
export function statusFor(
  installedVersion: string | null | undefined,
  latestVersion: string | null | undefined,
): InstallStatus | null {
  return installedVersion !== undefined ? computeStatus(installedVersion, latestVersion) : null;
}

// The download button's action for a given status (FC-22).
export function downloadAction(status: InstallStatus): DownloadAction {
  switch (status) {
    case "not-installed":
      return "install";
    case "update-available":
      return "update";
    case "installed":
      return "redownload";
  }
}
