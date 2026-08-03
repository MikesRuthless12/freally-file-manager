// The live download progress bar (FC-31): bound 0 → 100% to real bytes, with
// the two-decimal percent rendered beside it and mirrored into the accessible
// state. Every surface (card, detail view, Download All aggregate) uses this
// one component, so the visible percent and aria value can never drift.

import { useI18n } from "../i18n";
import { formatPercent } from "../releases/format";

interface ProgressBarProps {
  /** Progress as a fraction in [0, 1] — real bytes over published size,
   *  already clamped at the single source (downloads/progress.ts fraction). */
  fraction: number;
  /** Accessible name, e.g. "Download progress for Freally Capture". */
  label: string;
  /** Class for the percent text, so each surface keeps its own styling. */
  percentClassName: string;
}

export function ProgressBar({ fraction, label, percentClassName }: ProgressBarProps) {
  const { locale } = useI18n();
  const percent = fraction * 100;
  return (
    <>
      <div
        className="progress"
        role="progressbar"
        aria-label={label}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Number(percent.toFixed(2))}
      >
        <div className="progress-fill" style={{ width: `${percent}%` }} />
      </div>
      <span className={percentClassName}>{formatPercent(locale, fraction)}</span>
    </>
  );
}
