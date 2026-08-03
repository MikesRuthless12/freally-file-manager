import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AppAssets } from "../catalog/types";
import { loadRelease, normalizeRelease } from "./github";

const ASSETS: AppAssets = {
  windows: "\\.(exe|msi)$",
  macos: "\\.dmg$",
  linux: "\\.(AppImage|deb|rpm)$",
};

function releasePayload() {
  return {
    tag_name: "v1.2.3",
    name: "1.2.3",
    published_at: "2026-07-02T10:00:00Z",
    html_url: "https://github.com/acme/app/releases/tag/v1.2.3",
    body: "- Fixed things\n- Added stuff",
    assets: [
      { name: "App_1.2.3_x64-setup.exe", browser_download_url: "https://x/setup.exe", download_count: 100 },
      { name: "App_1.2.3.msi", browser_download_url: "https://x/app.msi", download_count: 50 },
      { name: "App_1.2.3.dmg", browser_download_url: "https://x/app.dmg", download_count: 30 },
      { name: "App_1.2.3.AppImage", browser_download_url: "https://x/app.AppImage", download_count: 20 },
      { name: "latest.json", browser_download_url: "https://x/latest.json", download_count: 5 },
    ],
  };
}

function okResponse(body: unknown): Response {
  return { ok: true, status: 200, json: async () => body } as unknown as Response;
}
function errResponse(status: number): Response {
  return { ok: false, status, json: async () => ({}) } as unknown as Response;
}

describe("normalizeRelease", () => {
  it("sums installers per OS and totals installers only (never latest.json)", () => {
    const info = normalizeRelease(releasePayload(), ASSETS);
    expect(info.tag).toBe("v1.2.3");
    expect(info.version).toBe("1.2.3");
    expect(info.notes).toContain("Fixed things");
    // Windows sums BOTH matching installers (.exe 100 + .msi 50).
    expect(info.perOs.windows).toBe(150);
    expect(info.perOs.macos).toBe(30);
    expect(info.perOs.linux).toBe(20);
    // Total = installers only (150 + 30 + 20); the latest.json's 5 is excluded
    // and the total reconciles with the per-OS rows.
    expect(info.totalDownloads).toBe(200);
  });

  it("returns null notes for an empty body and omits unmatched OSes", () => {
    const info = normalizeRelease(
      {
        tag_name: "v2.0.0",
        body: "  ",
        assets: [{ name: "readme.txt", browser_download_url: "u", download_count: 3 }],
      },
      ASSETS,
    );
    expect(info.notes).toBeNull();
    expect(info.perOs.windows).toBeUndefined();
    // A non-installer asset contributes nothing to the honest total.
    expect(info.totalDownloads).toBe(0);
  });

  it("tolerates a missing assets array and missing fields", () => {
    const info = normalizeRelease({ tag_name: "v0.1.0" }, undefined);
    expect(info.totalDownloads).toBe(0);
    expect(info.perOs).toEqual({});
    expect(info.version).toBe("0.1.0");
  });

  it("captures per-OS installer assets for the download engine (FC-30)", () => {
    const payload = releasePayload();
    // GitHub publishes exact sizes (and digests, when available) per asset.
    payload.assets = payload.assets.map((a) => ({
      ...a,
      size: 1000,
      digest: a.name.endsWith(".exe") ? "sha256:" + "b".repeat(64) : undefined,
    })) as typeof payload.assets;
    const info = normalizeRelease(payload, ASSETS);
    expect(info.installers.windows?.map((a) => a.name)).toEqual([
      "App_1.2.3_x64-setup.exe",
      "App_1.2.3.msi",
    ]);
    expect(info.installers.windows?.[0]).toEqual({
      name: "App_1.2.3_x64-setup.exe",
      url: "https://x/setup.exe",
      size: 1000,
      digest: "sha256:" + "b".repeat(64),
    });
    // No digest published → null (the engine then verifies size only).
    expect(info.installers.windows?.[1].digest).toBeNull();
    // A digest the engine can't verify (future sha512, malformed) degrades to
    // size-only the same way — it must never hard-fail the download.
    const odd = normalizeRelease(
      {
        tag_name: "v1.0.0",
        assets: [
          { name: "App.exe", browser_download_url: "u", download_count: 1, size: 9, digest: "sha512:" + "c".repeat(128) },
        ],
      },
      ASSETS,
    );
    expect(odd.installers.windows?.[0].digest).toBeNull();
    // latest.json never becomes an installer candidate.
    const all = Object.values(info.installers).flat();
    expect(all.some((a) => a.name === "latest.json")).toBe(false);
  });

  it("an asset without a usable URL or size is counted but not downloadable", () => {
    const info = normalizeRelease(
      {
        tag_name: "v1.0.0",
        assets: [{ name: "App.exe", download_count: 10 }], // no url/size
      },
      ASSETS,
    );
    expect(info.perOs.windows).toBe(10); // the honest count still shows
    expect(info.installers.windows).toEqual([]); // but nothing to stream
  });
});

describe("loadRelease", () => {
  beforeEach(() => {
    localStorage.clear();
  });
  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("resolves available and caches within the TTL (no second fetch)", async () => {
    const fetchMock = vi.fn().mockResolvedValue(okResponse(releasePayload()));
    vi.stubGlobal("fetch", fetchMock);
    const state = await loadRelease("acme/app", ASSETS);
    expect(state.status).toBe("available");
    const again = await loadRelease("acme/app", ASSETS);
    expect(again.status).toBe("available");
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("maps 404 to an honest 'none' (repo has no releases yet)", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(errResponse(404)));
    const state = await loadRelease("acme/empty", ASSETS);
    expect(state.status).toBe("none");
  });

  it("maps a rate-limit (403) to 'unavailable' and never caches it", async () => {
    const fetchMock = vi.fn().mockResolvedValue(errResponse(403));
    vi.stubGlobal("fetch", fetchMock);
    expect((await loadRelease("acme/app", ASSETS)).status).toBe("unavailable");
    // Not cached → the next call fetches again rather than serving a hidden state.
    await loadRelease("acme/app", ASSETS);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("maps a network error to 'unavailable'", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new Error("offline")));
    expect((await loadRelease("acme/app", ASSETS)).status).toBe("unavailable");
  });

  it("force bypasses a fresh cache", async () => {
    const fetchMock = vi.fn().mockResolvedValue(okResponse(releasePayload()));
    vi.stubGlobal("fetch", fetchMock);
    await loadRelease("acme/app", ASSETS);
    await loadRelease("acme/app", ASSETS, true);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });
});
