import type { AppAssets } from "../catalog/types";
import { OS_KEYS, type InstallerAsset, type OsKey, type ReleaseInfo, type ReleaseState } from "./types";

// Resolve each app's LIVE release data from GitHub (FC-10/FC-11). This mirrors
// the catalog loader: fetch from the frontend (the webview CSP allows https:),
// cache in localStorage to stay friendly to GitHub's unauthenticated rate limit
// (60/hour per IP), and NEVER fabricate a number — an unreachable API hides the
// counts instead (charter: honest numbers only).

const GITHUB_API = "https://api.github.com";
const CACHE_PREFIX = "fc.release.v1.";
// Cached results within this window are reused without a network call. Beyond it
// we re-fetch. ~6-7 apps refreshed every 15 min stays well under the rate limit.
export const CACHE_TTL_MS = 15 * 60 * 1000;

interface RawAsset {
  name?: unknown;
  download_count?: unknown;
  browser_download_url?: unknown;
  size?: unknown;
  digest?: unknown;
}

// The subset of the GitHub `releases/latest` payload we rely on.
interface RawRelease {
  tag_name?: unknown;
  published_at?: unknown;
  html_url?: unknown;
  body?: unknown;
  assets?: unknown;
}

function num(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}

function str(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

// Forward only a digest the engine can actually verify (sha256). Anything else
// (a future sha512, a malformed value) degrades to size-only verification —
// the same honest path assets publishing no digest already take — instead of
// hard-failing every download of that asset.
function usableDigest(value: unknown): string | null {
  const digest = str(value);
  return digest && /^sha256:[0-9a-fA-F]{64}$/.test(digest) ? digest : null;
}

// Every asset whose name matches an OS's installer regex — with the download
// count plus what the Phase 4 engine needs (URL, exact size, published digest).
// Summed/collected (not first-match) so a platform shipping several installers —
// Linux .AppImage + .deb + .rpm — reports its full figure. Returns undefined
// when the pattern is absent or matches nothing, so callers can omit that OS
// entirely. Invalid patterns are ignored (match nothing) rather than throwing.
function matchInstallers(
  assets: RawAsset[],
  pattern: string | undefined,
): { count: number; installers: InstallerAsset[] } | undefined {
  if (!pattern) return undefined;
  let re: RegExp;
  try {
    re = new RegExp(pattern);
  } catch {
    return undefined;
  }
  let count = 0;
  let matched = false;
  const installers: InstallerAsset[] = [];
  for (const asset of assets) {
    const name = str(asset.name);
    if (name && re.test(name)) {
      count += num(asset.download_count);
      matched = true;
      // Only a downloadable asset (real URL + positive size) becomes an
      // installer candidate; the count above still tallies regardless.
      const url = str(asset.browser_download_url);
      const size = num(asset.size);
      if (url && size > 0) {
        installers.push({ name, url, size, digest: usableDigest(asset.digest) });
      }
    }
  }
  return matched ? { count, installers } : undefined;
}

// Parse a GitHub `releases/latest` payload into our ReleaseInfo. `perOs` holds
// the summed installer downloads for each OS the manifest's patterns select, and
// `totalDownloads` is their sum — installers only, so updater and signature
// traffic never inflate the honest figure and the Total reconciles with the rows.
export function normalizeRelease(raw: RawRelease, assets: AppAssets | undefined): ReleaseInfo {
  const rawAssets: RawAsset[] = Array.isArray(raw.assets) ? (raw.assets as RawAsset[]) : [];

  const perOs: Partial<Record<OsKey, number>> = {};
  const installers: Partial<Record<OsKey, InstallerAsset[]>> = {};
  let totalDownloads = 0;
  for (const os of OS_KEYS) {
    const match = matchInstallers(rawAssets, assets?.[os]);
    if (match !== undefined) {
      perOs[os] = match.count;
      installers[os] = match.installers;
      totalDownloads += match.count;
    }
  }

  const tag = str(raw.tag_name) ?? "";
  const body = str(raw.body)?.trim();
  return {
    tag,
    version: tag.replace(/^v/i, "") || tag,
    publishedAt: str(raw.published_at) ?? "",
    htmlUrl: str(raw.html_url) ?? "",
    notes: body || null,
    perOs,
    installers,
    totalDownloads,
  };
}

// Cache the raw release payload (or a "none" marker) rather than the parsed
// result, so a later change to the manifest's asset patterns is reflected the
// next time we read the cache — not frozen until the TTL expires. "unavailable"
// is never cached, so an outage can't poison the cache with a hidden state.
type CacheEntry =
  | { fetchedAt: number; kind: "available"; raw: RawRelease }
  | { fetchedAt: number; kind: "none" };

function cacheKey(repo: string): string {
  return CACHE_PREFIX + repo;
}

function readCache(repo: string): CacheEntry | null {
  try {
    const rawText = localStorage.getItem(cacheKey(repo));
    if (!rawText) return null;
    const entry = JSON.parse(rawText) as CacheEntry;
    if (typeof entry?.fetchedAt !== "number") return null;
    if (entry.kind === "available" && entry.raw && typeof entry.raw === "object") return entry;
    if (entry.kind === "none") return entry;
  } catch {
    /* bad/absent cache — ignore */
  }
  return null;
}

function writeCache(repo: string, entry: CacheEntry): void {
  try {
    localStorage.setItem(cacheKey(repo), JSON.stringify(entry));
  } catch {
    /* storage full/unavailable — non-fatal */
  }
}

type FetchResult =
  | { kind: "available"; raw: RawRelease }
  | { kind: "none" }
  | { kind: "unavailable" };

// One network resolution for a repo, with no caching. 404 = the repo simply has
// no releases yet (honest "none"); rate-limit/offline/other errors = "unavailable".
async function fetchReleaseRaw(repo: string): Promise<FetchResult> {
  let res: Response;
  try {
    res = await fetch(`${GITHUB_API}/repos/${repo}/releases/latest`, {
      headers: { Accept: "application/vnd.github+json" },
    });
  } catch {
    return { kind: "unavailable" };
  }
  if (res.ok) {
    try {
      return { kind: "available", raw: (await res.json()) as RawRelease };
    } catch {
      return { kind: "unavailable" };
    }
  }
  if (res.status === 404) return { kind: "none" };
  // 403/429 = rate limited; any other non-OK status is treated as unreachable.
  return { kind: "unavailable" };
}

// Resolve a repo's release, reusing a fresh cached payload to spare the rate
// limit. `force` bypasses the cache (used by the manual/auto refresh). The parse
// (which applies the current `assets` patterns) always runs on read, so counts
// track the latest manifest even when served from cache.
export async function loadRelease(
  repo: string,
  assets: AppAssets | undefined,
  force = false,
): Promise<ReleaseState> {
  if (!force) {
    const cached = readCache(repo);
    if (cached && Date.now() - cached.fetchedAt < CACHE_TTL_MS) {
      return cached.kind === "available"
        ? { status: "available", release: normalizeRelease(cached.raw, assets) }
        : { status: "none" };
    }
  }
  const result = await fetchReleaseRaw(repo);
  if (result.kind === "available") {
    writeCache(repo, { fetchedAt: Date.now(), kind: "available", raw: result.raw });
    return { status: "available", release: normalizeRelease(result.raw, assets) };
  }
  if (result.kind === "none") {
    writeCache(repo, { fetchedAt: Date.now(), kind: "none" });
    return { status: "none" };
  }
  return { status: "unavailable" };
}
