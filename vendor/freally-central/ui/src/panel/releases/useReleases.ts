import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CatalogApp } from "../catalog/types";
import { CACHE_TTL_MS, loadRelease } from "./github";
import type { ReleaseState } from "./types";

// Freally Studio is excluded from the brand-wide download counter/rollout until
// its own roadmap exists (roadmap FC-51).
const STUDIO_ID = "freally-studio";
// Auto-refresh cadence (FC-11): re-fetch once a cached entry can have expired, so
// the rate-limit budget has a single source of truth.
const AUTO_REFRESH_MS = CACHE_TTL_MS;

// The manifest's `status` is the source of truth for availability, so only apps
// it marks "available" are queried. Coming-soon apps never show a version/count
// (which would contradict their pill) and never spend a request.
function isQueryable(app: CatalogApp): app is CatalogApp & { repo: string } {
  return Boolean(app.repo) && app.status === "available";
}

export interface ReleasesState {
  /** Resolved release state per app id (missing until the first fetch settles). */
  byId: Map<string, ReleaseState>;
  /** Brand-wide total downloads (Studio excluded); null when nothing real resolved. */
  grandTotal: number | null;
  loading: boolean;
  /** Force a re-fetch now, bypassing the cache. */
  refresh: () => void;
}

// Resolves live GitHub release data for every "available" catalog app that has a
// repo, keyed by app id. Honest by construction: an unreachable API yields
// "unavailable" and is omitted — counts are hidden, never faked.
export function useReleases(apps: CatalogApp[]): ReleasesState {
  const [byId, setById] = useState<Map<string, ReleaseState>>(new Map());
  const [loading, setLoading] = useState(false);
  // Bumps on every refresh; any value > 0 means "force past the cache".
  const [refreshCount, setRefreshCount] = useState(0);

  const repos = useMemo(
    () => apps.filter(isQueryable).map((a) => ({ id: a.id, repo: a.repo, assets: a.assets })),
    [apps],
  );
  // A value signature so the fetch effect re-runs when the queried repo set
  // changes by value — not merely on a new array identity, which a caller
  // re-rendering with a fresh apps array would otherwise trigger (a render loop).
  // The latest repos are read from a ref inside the effect.
  const reposSig = useMemo(
    () => JSON.stringify(repos.map((r) => [r.id, r.repo, r.assets ?? null])),
    [repos],
  );
  const reposRef = useRef(repos);
  reposRef.current = repos;

  const refresh = useCallback(() => setRefreshCount((n) => n + 1), []);

  useEffect(() => {
    const list = reposRef.current;
    if (list.length === 0) {
      setById(new Map());
      setLoading(false);
      return;
    }
    let active = true;
    setLoading(true);
    const force = refreshCount > 0;
    Promise.all(
      list.map(async (r) => [r.id, await loadRelease(r.repo, r.assets, force)] as const),
    ).then((entries) => {
      if (!active) return;
      setById(new Map(entries));
      setLoading(false);
    });
    return () => {
      active = false;
    };
  }, [reposSig, refreshCount]);

  useEffect(() => {
    const id = setInterval(refresh, AUTO_REFRESH_MS);
    return () => clearInterval(id);
  }, [refresh]);

  // The brand-wide total is only honest when the picture is complete: if any
  // (non-Studio) app is unavailable we hide it entirely rather than present a
  // partial sum as the whole. "none" apps (no releases yet) contribute a real 0.
  const grandTotal = useMemo(() => {
    let total = 0;
    let any = false;
    for (const [id, state] of byId) {
      if (id === STUDIO_ID) continue;
      if (state.status === "unavailable") return null;
      if (state.status === "available") {
        total += state.release.totalDownloads;
        any = true;
      }
    }
    return any ? total : null;
  }, [byId]);

  return { byId, grandTotal, loading, refresh };
}
