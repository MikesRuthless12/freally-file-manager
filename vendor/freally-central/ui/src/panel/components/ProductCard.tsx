import { useT } from "../i18n";
import { appTagline } from "../catalog/localize";
import type { CatalogApp } from "../catalog/types";
import { liveRelease, type ReleaseState } from "../releases/types";
import { statusFor } from "../install/status";
import type { AppDownloadState } from "../downloads/types";
import { isActive, isInstallActive, stateFraction } from "../downloads/progress";
import { AppIcon } from "./AppIcon";
import { ProgressBar } from "./ProgressBar";
import { StatusBadge } from "./StatusBadge";

interface ProductCardProps {
  app: CatalogApp;
  release?: ReleaseState;
  /** Detected installed version: string, null (probed & absent), or undefined (not probed). */
  installedVersion?: string | null;
  /** This app's download, when one ran this session (drives the card's bar). */
  download?: AppDownloadState;
  /** When false (view-only host), an available app's pill reads "Available", not "Download". */
  allowDownloads: boolean;
  onOpen: (app: CatalogApp) => void;
}

export function ProductCard({ app, release, installedVersion, download, allowDownloads, onOpen }: ProductCardProps) {
  const t = useT();
  const soon = app.status === "coming-soon";
  const tagline = appTagline(t, app);
  const live = liveRelease(release);
  // A badge only when detection produced a reading for this app (undefined means
  // not probed — coming-soon apps, or running outside the Tauri shell).
  const status = statusFor(installedVersion, live?.version);
  // The per-card live bar (FC-31/41) while this app downloads or installs.
  const downloading = isActive(download);
  const installing = isInstallActive(download);
  return (
    <button
      type="button"
      className={soon ? "card card--soon" : "card"}
      onClick={() => onOpen(app)}
    >
      <div className="card-head">
        <h3 className="card-name">{app.name}</h3>
        {tagline && <p className="card-tagline">{tagline}</p>}
      </div>
      <div className="card-art">
        <AppIcon app={app} size={150} />
      </div>
      <div className="card-foot">
        <div className="card-foot-row">
          <span className={soon ? "pill pill--soon" : "pill pill--view"}>
            {soon ? t("fcp-coming-soon") : t(allowDownloads ? "fcp-card-download" : "fcp-available")}
          </span>
          {status && <StatusBadge status={status} />}
        </div>
        {live && (
          <p className="card-meta">
            <span className="card-version">v{live.version}</span>
            {" · "}
            {/* Pass the number (not a preformatted string) so Fluent selects the
                right plural form and formats it for the active locale. */}
            {t("fcp-downloads-count", { count: live.totalDownloads })}
          </p>
        )}
        {(downloading || installing) && (
          <div className="card-progress">
            <ProgressBar
              fraction={stateFraction(download)}
              label={t(installing ? "fcp-install-progress-label" : "fcp-dl-progress-label", {
                name: app.name,
              })}
              percentClassName="card-progress-percent"
            />
          </div>
        )}
      </div>
    </button>
  );
}
