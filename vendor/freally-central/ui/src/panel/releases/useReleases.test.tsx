import { renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { CatalogApp } from "../catalog/types";
import type { ReleaseState } from "./types";
import { loadRelease } from "./github";
import { useReleases } from "./useReleases";

vi.mock("./github", () => ({
  loadRelease: vi.fn(),
  // Drives the auto-refresh interval; keep it long so it never fires mid-test.
  CACHE_TTL_MS: 15 * 60 * 1000,
}));

const mockLoad = vi.mocked(loadRelease);

function app(id: string, repo?: string, status: CatalogApp["status"] = "available"): CatalogApp {
  return { id, name: id, tagline: "", description: "", features: [], repo, status };
}

function available(total: number): ReleaseState {
  return {
    status: "available",
    release: { tag: "v1", version: "1", publishedAt: "", htmlUrl: "", notes: null, totalDownloads: total, perOs: {}, installers: {} },
  };
}

afterEach(() => vi.clearAllMocks());

describe("useReleases", () => {
  it("sums the brand-wide total but excludes Freally Studio (FC-51)", async () => {
    const totals: Record<string, number> = { "o/cap": 1000, "o/studio": 999 };
    mockLoad.mockImplementation((repo: string) => Promise.resolve(available(totals[repo] ?? 0)));

    const apps = [app("freally-capture", "o/cap"), app("freally-studio", "o/studio")];
    const { result } = renderHook(() => useReleases(apps));

    await waitFor(() => expect(result.current.grandTotal).not.toBeNull());
    expect(result.current.grandTotal).toBe(1000); // Studio's 999 is excluded
    // Studio's own card still resolves — only the brand counter drops it.
    expect(result.current.byId.get("freally-studio")?.status).toBe("available");
  });

  it("hides the grand total (null) when nothing real resolves", async () => {
    mockLoad.mockResolvedValue({ status: "unavailable" });
    const apps = [app("freally-capture", "o/cap")];
    const { result } = renderHook(() => useReleases(apps));

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.grandTotal).toBeNull();
  });

  it("does not query apps without a repo", async () => {
    mockLoad.mockResolvedValue(available(5));
    const apps = [app("freally-av")];
    renderHook(() => useReleases(apps));
    await waitFor(() => expect(mockLoad).not.toHaveBeenCalled());
  });

  it("does not query coming-soon apps even when they have a repo", async () => {
    mockLoad.mockResolvedValue(available(5));
    const apps = [app("freally-vault", "o/vault", "coming-soon")];
    renderHook(() => useReleases(apps));
    await waitFor(() => expect(mockLoad).not.toHaveBeenCalled());
  });

  it("hides the grand total during a partial outage rather than understating it", async () => {
    mockLoad.mockImplementation((repo: string) =>
      Promise.resolve(repo === "o/cap" ? available(1000) : { status: "unavailable" }),
    );
    const apps = [app("freally-capture", "o/cap"), app("freally-player", "o/player")];
    const { result } = renderHook(() => useReleases(apps));

    await waitFor(() => expect(result.current.loading).toBe(false));
    // Capture resolved (1000) but Player is unavailable → the brand-wide total is
    // hidden entirely, never presented as a partial figure.
    expect(result.current.grandTotal).toBeNull();
  });
});
