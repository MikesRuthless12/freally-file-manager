import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { isTauri } from "@tauri-apps/api/core";
import type { CatalogApp } from "../catalog/types";
import { liveRelease, type ReleaseState } from "../releases/types";
import { detectInstalled } from "./detect";
import { compareVersions } from "./semver";

// Only apps the manifest marks "available" can actually be on the machine, so
// coming-soon apps are never probed (they'd always read "not installed" and
// would spend a needless subprocess on Linux/macOS).
function isInstallable(app: CatalogApp): boolean {
  return app.status === "available";
}

export interface InstalledState {
  /**
   * Detected installed version per app id: a version string, or null when the
   * app was probed and found absent. A missing key means "not probed / unknown"
   * (coming-soon apps, or running outside the Tauri shell) — callers must treat
   * undefined differently from null so they never badge an app they didn't check.
   */
  byId: Map<string, string | null>;
  /** True only inside the Tauri shell, where native detection is possible. */
  supported: boolean;
  loading: boolean;
  /** Re-probe now (wired to the same manual Refresh as the release data). */
  refresh: () => void;
}

// Detects the installed version of every installable catalog app, keyed by id.
// Runs once when the app set settles and again on manual refresh — never on a
// timer (FC-20: "cached with a manual refresh"), and never on the UI thread
// (the Rust command does its blocking work off Tauri's worker).
export function useInstalled(apps: CatalogApp[]): InstalledState {
  const supported = isTauri();
  const [byId, setById] = useState<Map<string, string | null>>(new Map());
  const [loading, setLoading] = useState(false);
  const [refreshCount, setRefreshCount] = useState(0);

  const installable = useMemo(() => apps.filter(isInstallable), [apps]);
  // Re-probe when the installable set changes by VALUE (id + name — the two
  // fields detection uses), not merely on a new array identity from a re-render.
  const sig = useMemo(
    () => JSON.stringify(installable.map((a) => [a.id, a.name])),
    [installable],
  );
  const appsRef = useRef(installable);
  appsRef.current = installable;

  const refresh = useCallback(() => setRefreshCount((n) => n + 1), []);

  useEffect(() => {
    const list = appsRef.current;
    if (!supported || list.length === 0) {
      setById(new Map());
      setLoading(false);
      return;
    }
    let active = true;
    setLoading(true);
    detectInstalled(list).then((results) => {
      if (!active) return;
      setById(new Map(results.map((r) => [r.id, r.installedVersion])));
      setLoading(false);
    });
    return () => {
      active = false;
    };
  }, [sig, refreshCount, supported]);

  return { byId, supported, loading, refresh };
}

/**
 * The install map the UI surfaces actually read (FC-42): the real probe, with
 * this session's completed installs layered on top until the probe reads at
 * least the version that was installed. Honest on both sides — an
 * "installed ✓" here is either the installer's own record or this session's
 * real exit-0 install (which a re-probe, triggered here whenever an install
 * run settles, then confirms).
 */
export function useEffectiveInstalled(
  installed: InstalledState,
  releases: ReadonlyMap<string, ReleaseState>,
  downloads: { sessionInstalled: ReadonlySet<string>; installRunsCompleted: number },
): Map<string, string | null> {
  const { refresh, supported } = installed;
  const { sessionInstalled, installRunsCompleted } = downloads;

  useEffect(() => {
    if (installRunsCompleted > 0 && supported) refresh();
  }, [installRunsCompleted, supported, refresh]);

  return useMemo(() => {
    if (sessionInstalled.size === 0) return installed.byId;
    const merged = new Map(installed.byId);
    for (const id of sessionInstalled) {
      const live = liveRelease(releases.get(id));
      if (!live) continue;
      const probed = merged.get(id);
      if (probed == null || compareVersions(probed, live.version) < 0) {
        merged.set(id, live.version);
      }
    }
    return merged;
  }, [installed.byId, sessionInstalled, releases]);
}
