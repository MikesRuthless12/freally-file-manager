import { useState } from "react";
import { useI18n } from "../i18n";
import { openExternalUrl, useHost } from "../host";
import { launchApp } from "../downloads/engine";
import { appDescription, appFeatures, appTagline } from "../catalog/localize";
import type { CatalogApp } from "../catalog/types";
import { formatCount, formatDate } from "../releases/format";
import { liveRelease, type OsKey, type ReleaseState } from "../releases/types";
import { downloadAction, statusFor } from "../install/status";
import type { DownloadAction } from "../install/types";
import type { DownloadsApi } from "../downloads/useDownloads";
import {
  isActive,
  isCancelable,
  isInstallActive,
  isInstallReady,
  stateFraction,
} from "../downloads/progress";
import { failureMessageKey, installFailureMessageKey } from "../downloads/messages";
import { AppIcon } from "./AppIcon";
import { ChangelogView } from "./ChangelogView";
import { LiveRegion } from "./LiveRegion";
import { ProgressBar } from "./ProgressBar";
import { StatusBadge } from "./StatusBadge";

interface DetailViewProps {
  app: CatalogApp;
  release?: ReleaseState;
  /** Detected installed version: string, null (probed & absent), or undefined (not probed). */
  installedVersion?: string | null;
  downloads: DownloadsApi;
  /**
   * When false (view-only host), the Open / Install / Update / Download / Cancel
   * controls and the live progress panel are hidden; the description, features,
   * real download counts, and "What's New" changelog stay.
   */
  allowDownloads: boolean;
  onBack: () => void;
}

// Platform names are proper nouns shown identically in every locale.
const OS_ROWS: { key: OsKey; label: string }[] = [
  { key: "windows", label: "Windows" },
  { key: "macos", label: "macOS" },
  { key: "linux", label: "Linux" },
];

// The download button's label per action (FC-22).
const ACTION_KEY: Record<DownloadAction, string> = {
  install: "fcp-action-install",
  update: "fcp-action-update",
  redownload: "fcp-action-redownload",
};

export function DetailView({ app, release, installedVersion, downloads, allowDownloads, onBack }: DetailViewProps) {
  const { t, locale } = useI18n();
  const host = useHost();
  const [changelogOpen, setChangelogOpen] = useState(false);
  const [openFailed, setOpenFailed] = useState(false);
  const soon = app.status === "coming-soon";
  const tagline = appTagline(t, app);
  const description = appDescription(t, app);
  const features = appFeatures(t, app);
  const live = liveRelease(release);
  const releasedDate = live ? formatDate(locale, live.publishedAt) : null;
  // Install status (FC-21) — null when detection produced no reading for this app.
  const status = statusFor(installedVersion, live?.version);
  // The real download (FC-30): inside the shell the engine streams and verifies
  // this machine's installer with live progress. Outside it (plain browser), the
  // button falls back to opening the release page — the same verified source.
  const asset = downloads.installerFor(app, release);
  const download = downloads.byId.get(app.id);
  const engineReady = downloads.supported && asset !== null;
  const downloading = isActive(download);
  // The hands-off install stage (FC-41) — same panel, its own bar and label.
  const installing = isInstallActive(download);
  // isCancelable also covers an orphaned backend download ("alreadyDownloading"
  // after a reload) — Cancel must reach it or the user is locked out.
  const cancelable = isCancelable(download);
  const downloadUrl =
    live?.htmlUrl ?? app.site ?? (app.repo ? `https://github.com/${app.repo}/releases` : undefined);
  // The primary download CTA shows for any available app with a source, whether
  // or not detection has resolved — a failed or still-loading probe must never
  // hide it. Its label reflects detected status when known, else a plain Download.
  // A view-only host (allowDownloads=false) never shows it — the app's own site
  // becomes the primary CTA instead.
  const showDownload = allowDownloads && !soon && (downloadUrl !== undefined || engineReady);
  const action: DownloadAction = status ? downloadAction(status) : "install";
  const downloadLabelKey = status ? ACTION_KEY[action] : "fcp-card-download";
  // Open (FC-42): the app is on this machine — detected, or installed just now.
  const showOpen = allowDownloads && downloads.supported && status !== null && status !== "not-installed";
  // The external page for an app. In view-only mode there is no in-app download,
  // so an available app with no explicit `site` falls back to its release page —
  // otherwise its detail would show counts but offer no way to reach it. Full
  // mode is unchanged (the download CTA covers that path), and coming-soon apps
  // never get a spurious link to an empty releases page.
  const siteUrl = app.site ?? (!allowDownloads && !soon ? downloadUrl : undefined);
  const startPrimary = () => {
    if (downloads.supported && asset) {
      // The button's label is the consent: "Install"/"Update" run the whole
      // hands-off flow (FC-40); a plain "Download" (status unknown) or
      // "Re-download" only fetches the verified installer.
      if (!status || action === "redownload") downloads.start(app.id, asset);
      else downloads.installFlow(app.id, asset);
    } else if (downloadUrl) {
      openExternalUrl(host, downloadUrl);
    }
  };
  const openApp = () => {
    setOpenFailed(false);
    launchApp(app.id, app.name).catch(() => setOpenFailed(true));
  };
  // The one honest terminal note per phase (tone + localized message).
  const note = (() => {
    switch (download?.phase) {
      case "done":
        return { tone: "ok", key: download.checksumVerified ? "fcp-dl-done-verified" : "fcp-dl-done-size-only" };
      case "installed":
        return { tone: "ok", key: "fcp-install-done" };
      case "installFailed":
        return { tone: "error", key: installFailureMessageKey(download.code) };
      case "installCanceled":
        return { tone: "", key: "fcp-install-canceled" };
      case "failed":
        return { tone: "error", key: failureMessageKey(download.code) };
      case "canceled":
        return { tone: "", key: "fcp-dl-canceled" };
      default:
        return null;
    }
  })();
  return (
    <section className="detail">
      {/* Announces the terminal download/install outcome (or an open failure);
          the streaming percent stays on the bar's role="progressbar". */}
      <LiveRegion
        message={note ? t(note.key) : openFailed ? t("fcp-open-failed", { name: app.name }) : ""}
      />

      <button type="button" className="btn btn-ghost detail-back" onClick={onBack}>
        ← {t("fcp-detail-back")}
      </button>

      <div className="detail-head">
        <AppIcon app={app} size={128} />
        <div className="detail-headtext">
          <h2 className="detail-name">{app.name}</h2>
          {tagline && <p className="detail-tagline">{tagline}</p>}
          <div className="detail-badges">
            <span className={soon ? "pill pill--soon" : "pill pill--view"}>
              {soon ? t("fcp-coming-soon") : t("fcp-available")}
            </span>
            {status && <StatusBadge status={status} />}
            {live && <span className="detail-version">v{live.version}</span>}
            {app.startedDate && (
              <span className="detail-started">{t("fcp-detail-started", { date: app.startedDate })}</span>
            )}
            {releasedDate && (
              <span className="detail-started">{t("fcp-detail-released", { date: releasedDate })}</span>
            )}
          </div>
        </div>
      </div>

      {description && <p className="detail-desc">{description}</p>}

      {live && (
        <>
          <h3 className="detail-section-title">{t("fcp-detail-downloads")}</h3>
          <dl className="detail-downloads">
            {OS_ROWS.map(({ key, label }) => {
              const count = live.perOs[key];
              if (count === undefined) return null;
              return (
                <div className="detail-download-row" key={key}>
                  <dt>{label}</dt>
                  <dd>{formatCount(locale, count)}</dd>
                </div>
              );
            })}
            <div className="detail-download-row detail-download-total">
              <dt>{t("fcp-downloads-total")}</dt>
              <dd>{formatCount(locale, live.totalDownloads)}</dd>
            </div>
          </dl>
        </>
      )}

      {release?.status === "unavailable" && (
        <p className="detail-muted detail-downloads-note">{t("fcp-downloads-unavailable")}</p>
      )}

      <h3 className="detail-section-title">{t("fcp-detail-features")}</h3>
      {features.length > 0 ? (
        <ul className="detail-features">
          {features.map((feature) => (
            <li key={feature}>{feature}</li>
          ))}
        </ul>
      ) : (
        <p className="detail-muted">{t("fcp-detail-no-features")}</p>
      )}

      {soon && <p className="detail-note">{t("fcp-detail-coming-soon-note")}</p>}

      {(live || siteUrl || showDownload || showOpen) && (
        <div className="detail-actions">
          {showOpen && (
            <button type="button" className="btn btn-primary" onClick={openApp}>
              {t("fcp-open-app")}
            </button>
          )}
          {live && (
            <button type="button" className="btn" onClick={() => setChangelogOpen(true)}>
              {t("fcp-detail-whats-new")}
            </button>
          )}
          {siteUrl && (
            <button
              type="button"
              className={showDownload || showOpen ? "btn" : "btn btn-primary"}
              onClick={() => openExternalUrl(host, siteUrl)}
            >
              {t("fcp-detail-visit-site")}
            </button>
          )}
          {showDownload && !downloading && !installing && (
            <button
              type="button"
              className={showOpen ? "btn" : "btn btn-primary"}
              onClick={startPrimary}
            >
              {t(
                download?.phase === "failed" || download?.phase === "canceled"
                  ? "fcp-dl-retry"
                  : downloadLabelKey,
              )}
            </button>
          )}
          {cancelable && (
            <button type="button" className="btn" onClick={() => downloads.cancel(app.id)}>
              {t("fcp-dl-cancel")}
            </button>
          )}
        </div>
      )}

      {openFailed && <p className="detail-download-note detail-download-note--error">{t("fcp-open-failed", { name: app.name })}</p>}

      {/* The live download/install panel (FC-31 + FC-41): a real-bytes bar with
          the two-decimal percent while streaming/verifying, the staged install
          bar while the installer runs, then an honest terminal note. */}
      {download && (
        <div className="detail-download">
          {downloading && (
            <>
              <span className="detail-download-phase">
                {t(download.phase === "verifying" ? "fcp-dl-verifying" : "fcp-dl-downloading")}
              </span>
              <ProgressBar
                fraction={stateFraction(download)}
                label={t("fcp-dl-progress-label", { name: app.name })}
                percentClassName="detail-download-percent"
              />
            </>
          )}
          {installing && (
            <>
              <span className="detail-download-phase">{t("fcp-dl-installing")}</span>
              <ProgressBar
                fraction={stateFraction(download)}
                label={t("fcp-install-progress-label", { name: app.name })}
                percentClassName="detail-download-percent"
              />
            </>
          )}
          {note && (
            <p
              className={`detail-download-note${
                note.tone ? ` detail-download-note--${note.tone}` : ""
              }`}
            >
              {t(note.key)}
            </p>
          )}
          {/* The verified installer is still on disk after a failed or canceled
              install — keep it reachable so the user can always run it by hand.
              Hidden when the host has no reveal capability. */}
          {isInstallReady(download) && download.path && (
            <>
              <span className="detail-download-file">{download.path.split(/[\\/]/).pop()}</span>
              {host.revealInFolder && (
                <button
                  type="button"
                  className="btn"
                  onClick={() => void host.revealInFolder?.(download.path)}
                >
                  {t("fcp-dl-show-in-folder")}
                </button>
              )}
            </>
          )}
        </div>
      )}

      {changelogOpen && live && (
        <ChangelogView app={app} release={live} onClose={() => setChangelogOpen(false)} />
      )}
    </section>
  );
}
