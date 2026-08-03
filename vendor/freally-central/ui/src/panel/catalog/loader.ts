import { useEffect, useState } from "react";
import fallbackData from "./fallback.json";
import type { AppStatus, Catalog, CatalogApp } from "./types";

// Hosted manifest is the source of truth; a bundled copy guarantees the grid
// always renders (FC-02). Never hard-code product data in components.
const MANIFEST_URL =
  "https://mikesruthless12.github.io/freally-central/freally-central.json";
const CACHE_KEY = "fc.catalog.cache.v1";

export type CatalogSource = "hosted" | "cache" | "bundled";

function str(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

function coerceApp(raw: unknown): CatalogApp | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;
  const id = str(o.id);
  if (!id) return null; // skip $comment / malformed entries
  const status: AppStatus = o.status === "coming-soon" ? "coming-soon" : "available";
  const assets =
    o.assets && typeof o.assets === "object"
      ? (o.assets as Record<string, unknown>)
      : undefined;
  return {
    id,
    name: str(o.name) ?? id,
    tagline: str(o.tagline) ?? "",
    description: str(o.description) ?? "",
    features: Array.isArray(o.features)
      ? o.features.filter((f): f is string => typeof f === "string")
      : [],
    icon: str(o.icon),
    startedDate: str(o.startedDate),
    repo: str(o.repo),
    site: str(o.site),
    changelog: str(o.changelog),
    status,
    assets: assets
      ? { windows: str(assets.windows), macos: str(assets.macos), linux: str(assets.linux) }
      : undefined,
  };
}

export function normalizeCatalog(raw: unknown): Catalog {
  const root = (raw && typeof raw === "object" ? raw : {}) as Record<string, unknown>;
  const apps = Array.isArray(root.apps)
    ? (root.apps.map(coerceApp).filter((a): a is CatalogApp => a !== null))
    : [];
  return {
    schemaVersion: typeof root.schemaVersion === "number" ? root.schemaVersion : 1,
    brand: str(root.brand) ?? "Freally",
    apps,
  };
}

export const bundledCatalog: Catalog = normalizeCatalog(fallbackData);

// A host may mount the panel in a closable dialog, re-running useCatalog on
// every open — serve a recent hosted fetch from memory instead of re-hitting
// the network each time (same TTL idiom as releases/github.ts). Only hosted
// successes memoize: an offline fallback must not suppress the next retry.
const CATALOG_TTL_MS = 15 * 60 * 1000;
let lastHosted: { at: number; catalog: Catalog } | null = null;

export async function loadCatalog(): Promise<{ catalog: Catalog; source: CatalogSource }> {
  if (lastHosted && Date.now() - lastHosted.at < CATALOG_TTL_MS) {
    return { catalog: lastHosted.catalog, source: "hosted" };
  }
  try {
    const res = await fetch(MANIFEST_URL, { headers: { Accept: "application/json" } });
    if (res.ok) {
      const data: unknown = await res.json();
      const catalog = normalizeCatalog(data);
      if (catalog.apps.length > 0) {
        try {
          localStorage.setItem(CACHE_KEY, JSON.stringify(data));
        } catch {
          /* storage full/unavailable — non-fatal */
        }
        lastHosted = { at: Date.now(), catalog };
        return { catalog, source: "hosted" };
      }
    }
  } catch {
    /* offline or blocked — fall through to cache/bundled */
  }
  try {
    const cached = localStorage.getItem(CACHE_KEY);
    if (cached) {
      const catalog = normalizeCatalog(JSON.parse(cached) as unknown);
      if (catalog.apps.length > 0) return { catalog, source: "cache" };
    }
  } catch {
    /* bad cache — ignore */
  }
  return { catalog: bundledCatalog, source: "bundled" };
}

export interface CatalogState {
  apps: CatalogApp[];
  source: CatalogSource;
  brand: string;
  /** false until the hosted/cache/bundled resolution completes (avoids an offline-note flash). */
  loaded: boolean;
}

// Renders the bundled catalog instantly, then upgrades to the hosted copy.
export function useCatalog(): CatalogState {
  const [state, setState] = useState<CatalogState>({
    apps: bundledCatalog.apps,
    source: "bundled",
    brand: bundledCatalog.brand,
    loaded: false,
  });

  useEffect(() => {
    let active = true;
    loadCatalog().then(({ catalog, source }) => {
      if (active) setState({ apps: catalog.apps, source, brand: catalog.brand, loaded: true });
    });
    return () => {
      active = false;
    };
  }, []);

  return state;
}
