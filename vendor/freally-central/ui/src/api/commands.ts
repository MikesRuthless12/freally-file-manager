import { invoke } from "@tauri-apps/api/core";
import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
import { isSafeHttpUrl } from "../panel";
import type { BuildInfo, ReleaseNotes } from "./types";

// Build metadata for the About panel (from src-tauri/src/commands.rs).
export function buildInfo(): Promise<BuildInfo> {
  return invoke<BuildInfo>("build_info");
}

// The running build's changelog section for What's New (embedded from CHANGELOG.md).
export function releaseNotes(): Promise<ReleaseNotes> {
  return invoke<ReleaseNotes>("release_notes");
}

/**
 * A plain `<a target="_blank">` doesn't reach the OS browser from a Tauri
 * webview, so hand the URL to the opener plugin. Falls back to window.open when
 * running in a plain browser (dev/test).
 *
 * Only http(s) URLs are ever opened: these links come from the catalog manifest
 * and the GitHub API, and restricting the scheme keeps a stray `file:`/other
 * link from being handed to the OS opener (defense in depth).
 */
/**
 * Reveal a downloaded file in the OS file manager (Explorer/Finder/…), so the
 * user can reach the verified installer until Phase 5 runs it hands-off.
 * No-ops outside the Tauri shell.
 */
export async function revealInFolder(path: string): Promise<void> {
  try {
    await revealItemInDir(path);
  } catch {
    /* plain browser or reveal unsupported — nothing to do */
  }
}

export async function openExternal(url: string): Promise<void> {
  if (!isSafeHttpUrl(url)) return;
  try {
    await openUrl(url);
  } catch {
    window.open(url, "_blank", "noopener");
  }
}
