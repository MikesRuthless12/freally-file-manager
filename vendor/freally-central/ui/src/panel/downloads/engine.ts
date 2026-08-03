// The engine boundary: production downloads run in the Rust backend
// (crates/freally-central-engine/src/download.rs) over an IPC channel; tests inject a scripted
// engine + platform via window.__FC_TEST__ so the download flows are drivable
// outside the Tauri shell (vitest, Playwright). The injection is test-only —
// nothing in the app ever sets it.

import { Channel, invoke, isTauri } from "@tauri-apps/api/core";
import { OS_KEYS, type OsKey } from "../releases/types";
import type {
  DownloadEngine,
  DownloadEvent,
  DownloadRequest,
  InstallEvent,
  InstallRequest,
} from "./types";

export interface Platform {
  os: OsKey;
  arch: string; // Rust std::env::consts::ARCH, e.g. "x86_64"
}

interface TestHooks {
  downloadEngine?: DownloadEngine;
  platform?: Platform;
}

declare global {
  interface Window {
    /** Test-only injection point (Playwright/vitest); never set in production. */
    __FC_TEST__?: TestHooks;
  }
}

const tauriEngine: DownloadEngine = {
  async start(request: DownloadRequest, onEvent: (event: DownloadEvent) => void): Promise<void> {
    const channel = new Channel<DownloadEvent>();
    channel.onmessage = onEvent;
    await invoke("central_start_download", { request, onEvent: channel });
  },
  cancel(id: string): void {
    void invoke("central_cancel_download", { id });
  },
  async install(requests: InstallRequest[], onEvent: (event: InstallEvent) => void): Promise<void> {
    const channel = new Channel<InstallEvent>();
    channel.onmessage = onEvent;
    await invoke("central_install_apps", { requests, onEvent: channel });
  },
  cancelInstalls(): void {
    void invoke("central_cancel_installs");
  },
};

/**
 * Launch an installed catalog app (FC-42 "Open"). The backend only launches
 * what an installer actually recorded on this machine — it rejects when the
 * app isn't found, so callers must surface that honestly.
 */
export function launchApp(id: string, name: string): Promise<void> {
  return invoke("central_launch_app", { id, name });
}

/** The active engine, or null when downloads aren't possible here (plain browser). */
export function getEngine(): DownloadEngine | null {
  const injected = window.__FC_TEST__?.downloadEngine;
  if (injected) return injected;
  return isTauri() ? tauriEngine : null;
}

/**
 * This machine's OS + arch, for installer selection — from the engine's
 * platform probe inside the shell, or null in a plain browser (downloads are
 * off there).
 */
export async function getPlatform(): Promise<Platform | null> {
  const injected = window.__FC_TEST__?.platform;
  if (injected) return injected;
  if (!isTauri()) return null;
  try {
    const info = await invoke<{ os: string; arch: string }>("central_platform");
    const os = OS_KEYS.find((k) => k === info.os);
    return os ? { os, arch: info.arch } : null;
  } catch {
    return null;
  }
}
