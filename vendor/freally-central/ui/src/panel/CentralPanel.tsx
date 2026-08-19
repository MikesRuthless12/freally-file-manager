// The "Central inside" embeddable panel (FC-50): the catalog grid, detail
// view, and the whole download → verify → silent-install flow, as one
// self-contained component. Hosts provide i18n (their own Fluent runtime with
// the panel's fcp-* catalogs loaded) and the shell actions; everything else —
// manifest, live release data, install detection, downloads, installs — is the
// same single implementation Freally Central itself runs.
import { useCallback, useMemo, useState } from "react";
import { useCatalog } from "./catalog/loader";
import { CardGrid } from "./components/CardGrid";
import { DetailView } from "./components/DetailView";
import { Toolbar, type Filter } from "./components/Toolbar";
import type { BatchEntry } from "./downloads/types";
import { useDownloads } from "./downloads/useDownloads";
import { PanelHostContext, type PanelHost } from "./host";
import { PanelI18nProvider, type Translate } from "./i18n";
import { useEffectiveInstalled, useInstalled } from "./install/useInstalled";
import { formatCount } from "./releases/format";
import { useReleases } from "./releases/useReleases";
import type { CatalogApp } from "./catalog/types";
import "./panel.css";

export interface CentralPanelProps {
  /** The host's translate function; must resolve the panel's `fcp-*` keys. */
  t: Translate;
  /** The host's active locale (BCP 47), for number/date formatting. */
  locale: string;
  /** Shell actions only the host can perform. */
  host: PanelHost;
  /**
   * When false, the panel is a view-only showcase: every download / install /
   * Download-All control is hidden, while the cards, detail view, live release
   * data, real download counts, and the changelog viewer all stay. Defaults to
   * true, so a host that ships downloads (Central itself, Freally Capture) is
   * unchanged; hosts that only surface the catalog pass false.
   */
  allowDownloads?: boolean;
}

function matchesFilter(app: CatalogApp, filter: Filter): boolean {
  return filter === "all" ? true : app.status === filter;
}

// The full catalog renders in every host — including the host app itself,
// whose card then honestly shows Installed ✓ / Update available. Hiding apps
// here would also skew the brand-wide download total, which must stay real.
export function CentralPanel({ t, locale, host, allowDownloads = true }: CentralPanelProps) {
  const catalog = useCatalog();
  const apps = catalog.apps;
  const releases = useReleases(apps);
  const installed = useInstalled(apps);
  // useDownloads owns the engine channels AND their teardown: unmounting the
  // panel (a host may render it in a closable dialog) cancels in-flight work
  // so nothing runs unobserved.
  const downloads = useDownloads();

  // One manual Refresh re-checks both the live release data and what's installed.
  const refreshAll = useCallback(() => {
    releases.refresh();
    installed.refresh();
  }, [releases, installed]);
  const [filter, setFilter] = useState<Filter>("all");
  const [selectedId, setSelectedId] = useState<string | null>(null);

  const selected = apps.find((a) => a.id === selectedId) ?? null;
  const visible = useMemo(() => apps.filter((a) => matchesFilter(a, filter)), [apps, filter]);

  // The install map the cards and detail view read: the probe merged with this
  // session's real installs (the hook also re-probes when install runs settle).
  const mergedInstalled = useEffectiveInstalled(installed, releases.byId, downloads);

  // Everything Download & install all would fetch: each available app whose
  // release ships an installer for this machine (FC-32). Depends on
  // installerFor (a stable callback that changes only with the platform), NOT
  // the whole downloads object — which changes identity on every progress event.
  const { installerFor } = downloads;
  const downloadAllEntries = useMemo<BatchEntry[]>(() => {
    // A view-only host never renders Download-All, so skip the per-app installer
    // scan entirely — the only reason installerFor would run in such an embed.
    if (!allowDownloads) return [];
    const entries: BatchEntry[] = [];
    for (const app of apps) {
      const asset = installerFor(app, releases.byId.get(app.id));
      if (asset) entries.push({ appId: app.id, asset });
    }
    return entries;
  }, [apps, releases.byId, installerFor, allowDownloads]);

  return (
    <PanelI18nProvider t={t} locale={locale}>
      <PanelHostContext.Provider value={host}>
        <div className="fcp-root">
          {selected ? (
            <DetailView
              app={selected}
              release={releases.byId.get(selected.id)}
              installedVersion={mergedInstalled.get(selected.id)}
              downloads={downloads}
              allowDownloads={allowDownloads}
              onBack={() => setSelectedId(null)}
            />
          ) : (
            <>
              <Toolbar
                filter={filter}
                onFilter={setFilter}
                downloads={downloads}
                entries={downloadAllEntries}
                allowDownloads={allowDownloads}
              />
              {catalog.loaded && catalog.source === "bundled" && (
                <p className="offline-note">{t("fcp-status-offline")}</p>
              )}
              <div className="brand-total">
                {releases.grandTotal !== null && (
                  <span className="brand-total-count">
                    {t("fcp-downloads-brandwide", {
                      count: formatCount(locale, releases.grandTotal),
                    })}
                  </span>
                )}
                <button
                  type="button"
                  className="link-btn"
                  onClick={refreshAll}
                  disabled={releases.loading || installed.loading}
                >
                  {t("fcp-refresh")}
                </button>
              </div>
              <CardGrid
                apps={visible}
                releases={releases.byId}
                installed={mergedInstalled}
                downloads={downloads.byId}
                allowDownloads={allowDownloads}
                onOpen={(a) => setSelectedId(a.id)}
              />
            </>
          )}
        </div>
      </PanelHostContext.Provider>
    </PanelI18nProvider>
  );
}
