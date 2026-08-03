import { useT } from "../i18n";
import type { BatchEntry } from "../downloads/types";
import type { BatchSummary, DownloadsApi } from "../downloads/useDownloads";
import { batchProgress, installProgress } from "../downloads/progress";
import { LiveRegion } from "./LiveRegion";
import { ProgressBar } from "./ProgressBar";

export type Filter = "all" | "available" | "coming-soon";

interface ToolbarProps {
  filter: Filter;
  onFilter: (filter: Filter) => void;
  downloads: DownloadsApi;
  /** Every installer Download & install all would fetch on this machine. */
  entries: BatchEntry[];
  /** When false (view-only host), the Download-All control and its progress are hidden. */
  allowDownloads: boolean;
}

export function Toolbar({ filter, onFilter, downloads, entries, allowDownloads }: ToolbarProps) {
  const t = useT();
  const { batch } = downloads;
  const busy = batch.status === "running" || batch.status === "installing";

  // Download & install all (FC-32 + FC-40): enabled only where a real engine
  // exists and at least one app ships an installer for this machine; the
  // title says why otherwise.
  const canStart = downloads.supported && entries.length > 0 && !busy;
  const hint = !downloads.supported
    ? t("fcp-install-all-unsupported")
    : entries.length === 0
      ? t("fcp-install-all-none")
      : t("fcp-install-all-hint");

  // The live aggregate bar reflects the batch that was actually queued; the
  // lifecycle comes from the store, not re-derived from per-entry states. Each
  // stage's aggregate is computed only while that stage renders it.
  let liveBar: { label: string; fraction: number } | null = null;
  if (batch.status === "running") {
    const progress = batchProgress(batch.entries, downloads.byId);
    liveBar = {
      label: t("fcp-batch-progress", { done: progress.done, total: batch.entries.length }),
      fraction: progress.fraction,
    };
  } else if (batch.status === "installing") {
    const installs = installProgress(batch.installIds, downloads.byId);
    liveBar = {
      label: t("fcp-batch-installing", { done: installs.installed, total: batch.installIds.length }),
      fraction: installs.fraction,
    };
  }

  // The settled line reports EVERY problem the batch had — a failed install
  // must never hide a download that was never fetched, and vice versa. It
  // reads from the summary frozen at settle time, so later activity on a
  // member app can't rewrite what this batch's outcome was.
  const settledLabel = (s: BatchSummary): string => {
    const entriesTotal = batch.entries.length;
    const installTotal = batch.installIds.length;
    const parts: string[] = [];
    if (s.downloadsFailed > 0) {
      parts.push(t("fcp-batch-failed", { failed: s.downloadsFailed, total: entriesTotal }));
    }
    if (s.installsFailed > 0) {
      parts.push(t("fcp-batch-install-failed", { failed: s.installsFailed, total: installTotal }));
    }
    if (s.downloadsCanceled > 0) {
      parts.push(t("fcp-batch-canceled", { canceled: s.downloadsCanceled, total: entriesTotal }));
    }
    if (s.installsCanceled > 0) {
      parts.push(
        t("fcp-batch-install-canceled", { canceled: s.installsCanceled, total: installTotal }),
      );
    }
    if (parts.length > 0) return parts.join(" ");
    return installTotal > 0 ? t("fcp-batch-installed") : t("fcp-batch-done");
  };

  const settled = batch.status === "settled" && batch.summary ? settledLabel(batch.summary) : "";

  return (
    <div className="toolbar">
      {/* Announces the Download-All outcome once the batch settles (the running
          % is conveyed by the bar, not here). */}
      <LiveRegion message={settled} />

      <div className="toolbar-row">
        {allowDownloads && (
          <button
            type="button"
            className="download-all"
            disabled={!canStart}
            title={hint}
            onClick={() => downloads.startAll(entries)}
          >
            {t("fcp-install-all")}
          </button>
        )}
        <label className="type-filter">
          <span className="type-filter-label">{t("fcp-filter-type")}</span>
          <select
            value={filter}
            onChange={(e) => onFilter(e.target.value as Filter)}
            aria-label={t("fcp-filter-type")}
          >
            <option value="all">{t("fcp-filter-all")}</option>
            <option value="available">{t("fcp-available")}</option>
            <option value="coming-soon">{t("fcp-coming-soon")}</option>
          </select>
        </label>
      </div>

      {allowDownloads && batch.status !== "idle" && (
        <div className="batch">
          {liveBar && (
            <>
              <span className="batch-label">{liveBar.label}</span>
              <ProgressBar
                fraction={liveBar.fraction}
                label={t("fcp-install-all")}
                percentClassName="batch-percent"
              />
              <button type="button" className="btn btn-ghost" onClick={downloads.cancelAll}>
                {t("fcp-batch-cancel-all")}
              </button>
            </>
          )}
          {batch.status === "settled" && batch.summary && (
            <span className="batch-label">{settledLabel(batch.summary)}</span>
          )}
        </div>
      )}
    </div>
  );
}
