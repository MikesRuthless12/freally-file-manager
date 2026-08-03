import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { CatalogApp } from "../catalog/types";
import type { InstallerAsset, ReleaseState } from "../releases/types";
import type {
  DownloadEngine,
  DownloadEvent,
  DownloadRequest,
  InstallEvent,
  InstallRequest,
} from "./types";
import { MAX_CONCURRENT_DOWNLOADS, useDownloads } from "./useDownloads";

// Drive the store with a scripted engine injected through the test-only hook —
// the same seam the Playwright specs use, so no Tauri shell is needed.

function asset(id: string, size = 100): InstallerAsset {
  return {
    name: `${id}_1.0.0_x64-setup.exe`,
    url: `https://github.com/o/${id}/releases/download/v1/${id}_1.0.0_x64-setup.exe`,
    size,
    digest: "sha256:" + "a".repeat(64),
  };
}

/** An engine that succeeds/fails per app id (downloads and installs scripted
 *  separately) and records download concurrency + what it was asked to install. */
function scriptedEngine(failIds: Set<string> = new Set(), failInstallIds: Set<string> = new Set()) {
  let active = 0;
  let maxActive = 0;
  const installedIds: string[] = [];
  const engine: DownloadEngine = {
    async start(request: DownloadRequest, onEvent: (e: DownloadEvent) => void): Promise<void> {
      active += 1;
      maxActive = Math.max(maxActive, active);
      onEvent({ kind: "started", total: request.expectedSize, resumedFrom: 0 });
      await Promise.resolve(); // yield so downloads genuinely interleave
      onEvent({ kind: "progress", received: request.expectedSize / 2, total: request.expectedSize });
      if (failIds.has(request.id)) {
        onEvent({ kind: "failed", code: "network", detail: "scripted failure" });
      } else {
        onEvent({ kind: "progress", received: request.expectedSize, total: request.expectedSize });
        onEvent({ kind: "verifying" });
        onEvent({ kind: "done", path: `/tmp/${request.fileName}`, sha256: "a".repeat(64), checksumVerified: true });
      }
      active -= 1;
    },
    cancel: vi.fn(),
    async install(requests: InstallRequest[], onEvent: (e: InstallEvent) => void): Promise<void> {
      for (const { id } of requests) {
        onEvent({ kind: "started", id });
        await Promise.resolve();
        onEvent({ kind: "progress", id, fraction: 0.5 });
        if (failInstallIds.has(id)) {
          onEvent({ kind: "failed", id, code: "installerFailed", detail: "scripted failure" });
        } else {
          installedIds.push(id);
          onEvent({ kind: "installed", id });
        }
      }
    },
    cancelInstalls: vi.fn(),
  };
  return { engine, maxConcurrent: () => maxActive, installedIds };
}

afterEach(() => {
  delete window.__FC_TEST__;
});

describe("useDownloads", () => {
  it("is unsupported outside the shell (no engine, no test injection)", () => {
    const { result } = renderHook(() => useDownloads());
    expect(result.current.supported).toBe(false);
  });

  it("drives one download from events to a verified done state", async () => {
    const { engine } = scriptedEngine();
    window.__FC_TEST__ = { downloadEngine: engine };
    const { result } = renderHook(() => useDownloads());
    expect(result.current.supported).toBe(true);

    await act(async () => {
      result.current.start("freally-capture", asset("freally-capture"));
    });

    await waitFor(() => {
      expect(result.current.byId.get("freally-capture")).toEqual({
        phase: "done",
        path: "/tmp/freally-capture_1.0.0_x64-setup.exe",
        checksumVerified: true,
      });
    });
  });

  it("Download & install all isolates failures and installs the rest (FC-32+FC-40)", async () => {
    const { engine, installedIds } = scriptedEngine(new Set(["bad"]));
    window.__FC_TEST__ = { downloadEngine: engine };
    const { result } = renderHook(() => useDownloads());

    await act(async () => {
      result.current.startAll([
        { appId: "good-1", asset: asset("good-1") },
        { appId: "bad", asset: asset("bad") },
        { appId: "good-2", asset: asset("good-2") },
      ]);
    });

    await waitFor(() => {
      expect(result.current.batch.status).toBe("settled");
    });
    // The verified set continued into its silent install; the failed download
    // never reached the installer (only verified files may execute).
    expect(result.current.byId.get("good-1")?.phase).toBe("installed");
    expect(result.current.byId.get("good-2")?.phase).toBe("installed");
    expect(result.current.batch.installIds).toEqual(["good-1", "good-2"]);
    expect(installedIds).toEqual(["good-1", "good-2"]);
    expect(result.current.sessionInstalled).toEqual(new Set(["good-1", "good-2"]));
    // The failure keeps the real bytes it transferred (50 of 100).
    expect(result.current.byId.get("bad")).toEqual({ phase: "failed", code: "network", received: 50 });
  });

  it("bounds Download All concurrency", async () => {
    const { engine, maxConcurrent } = scriptedEngine();
    window.__FC_TEST__ = { downloadEngine: engine };
    const { result } = renderHook(() => useDownloads());

    const entries = Array.from({ length: 6 }, (_, i) => ({
      appId: `app-${i}`,
      asset: asset(`app-${i}`),
    }));
    await act(async () => {
      result.current.startAll(entries);
    });

    await waitFor(() => {
      expect(result.current.batch.status).toBe("settled");
    });
    expect(maxConcurrent()).toBeLessThanOrEqual(MAX_CONCURRENT_DOWNLOADS);
    for (const entry of entries) {
      expect(result.current.byId.get(entry.appId)?.phase).toBe("installed");
    }
  });

  it("installFlow chains one download into its silent install (FC-40)", async () => {
    const { engine, installedIds } = scriptedEngine();
    window.__FC_TEST__ = { downloadEngine: engine };
    const { result } = renderHook(() => useDownloads());

    await act(async () => {
      result.current.installFlow("freally-capture", asset("freally-capture"));
    });

    await waitFor(() => {
      expect(result.current.byId.get("freally-capture")?.phase).toBe("installed");
    });
    expect(installedIds).toEqual(["freally-capture"]);
    expect(result.current.sessionInstalled.has("freally-capture")).toBe(true);
    // The install phases carry the finished download forward.
    expect(result.current.byId.get("freally-capture")).toEqual({
      phase: "installed",
      path: "/tmp/freally-capture_1.0.0_x64-setup.exe",
      checksumVerified: true,
    });
    expect(result.current.installRunsCompleted).toBe(1);
  });

  it("a failed download never reaches the installer via installFlow", async () => {
    const { engine, installedIds } = scriptedEngine(new Set(["bad"]));
    window.__FC_TEST__ = { downloadEngine: engine };
    const { result } = renderHook(() => useDownloads());

    await act(async () => {
      result.current.installFlow("bad", asset("bad"));
    });

    await waitFor(() => {
      expect(result.current.byId.get("bad")?.phase).toBe("failed");
    });
    expect(installedIds).toEqual([]);
    expect(result.current.installRunsCompleted).toBe(0);
  });

  it("reports an install failure honestly and keeps the downloaded file offered", async () => {
    const { engine } = scriptedEngine(new Set(), new Set(["freally-capture"]));
    window.__FC_TEST__ = { downloadEngine: engine };
    const { result } = renderHook(() => useDownloads());

    await act(async () => {
      result.current.installFlow("freally-capture", asset("freally-capture"));
    });

    await waitFor(() => {
      expect(result.current.byId.get("freally-capture")?.phase).toBe("installFailed");
    });
    expect(result.current.byId.get("freally-capture")).toEqual({
      phase: "installFailed",
      code: "installerFailed",
      path: "/tmp/freally-capture_1.0.0_x64-setup.exe",
      checksumVerified: true,
    });
    expect(result.current.sessionInstalled.has("freally-capture")).toBe(false);
  });

  it("cancelAll reaches the install stage", async () => {
    const { engine } = scriptedEngine();
    window.__FC_TEST__ = { downloadEngine: engine };
    const { result } = renderHook(() => useDownloads());

    act(() => {
      result.current.cancelAll();
    });
    expect(engine.cancelInstalls).toHaveBeenCalled();
  });

  it("resolves this machine's installer via the injected platform", async () => {
    const { engine } = scriptedEngine();
    window.__FC_TEST__ = {
      downloadEngine: engine,
      platform: { os: "windows", arch: "x86_64" },
    };
    const { result } = renderHook(() => useDownloads());

    const app: CatalogApp = {
      id: "freally-capture",
      name: "Freally Capture",
      tagline: "",
      description: "",
      features: [],
      status: "available",
    };
    const release: ReleaseState = {
      status: "available",
      release: {
        tag: "v1",
        version: "1",
        publishedAt: "",
        htmlUrl: "",
        notes: null,
        perOs: { windows: 5 },
        installers: { windows: [asset("freally-capture")] },
        totalDownloads: 5,
      },
    };

    await waitFor(() => {
      expect(result.current.installerFor(app, release)?.name).toBe(
        "freally-capture_1.0.0_x64-setup.exe",
      );
    });
    // Coming-soon apps and unknown releases never resolve an installer.
    expect(result.current.installerFor({ ...app, status: "coming-soon" }, release)).toBeNull();
    expect(result.current.installerFor(app, undefined)).toBeNull();
  });
});
