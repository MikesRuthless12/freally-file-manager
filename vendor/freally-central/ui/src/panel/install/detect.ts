import { invoke, isTauri } from "@tauri-apps/api/core";
import type { CatalogApp } from "../catalog/types";
import type { DetectResult } from "./types";

// Bridge to the engine's `central_detect_installed` command
// (crates/freally-central-engine/src/detect.rs).
//
// Detection is a native capability: outside the Tauri shell (the dev server, the
// Playwright web build, unit tests) there is no backend, so we return nothing
// rather than throw — the grid simply shows no install badges, which is honest.
export async function detectInstalled(apps: CatalogApp[]): Promise<DetectResult[]> {
  if (!isTauri()) return [];
  const query = apps.map((app) => ({ id: app.id, name: app.name }));
  if (query.length === 0) return [];
  try {
    return await invoke<DetectResult[]>("central_detect_installed", { apps: query });
  } catch {
    // A backend error must never break the grid; treat it as "unknown".
    return [];
  }
}
