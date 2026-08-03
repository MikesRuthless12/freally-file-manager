import { useT } from "../i18n";
import type { InstallStatus } from "../install/types";

// The install-status badge (FC-21): Installed ✓ / Update available / Not installed.
// Shared by the grid card and the detail view so both read identically.

const LABEL_KEY: Record<InstallStatus, string> = {
  installed: "fcp-status-installed",
  "update-available": "fcp-status-update",
  "not-installed": "fcp-status-not-installed",
};

const MODIFIER: Record<InstallStatus, string> = {
  installed: "status-badge--installed",
  "update-available": "status-badge--update",
  "not-installed": "status-badge--none",
};

export function StatusBadge({ status }: { status: InstallStatus }) {
  const t = useT();
  return (
    <span className={`status-badge ${MODIFIER[status]}`}>
      {status === "installed" && (
        <span aria-hidden="true" className="status-badge-check">
          ✓
        </span>
      )}
      {t(LABEL_KEY[status])}
    </span>
  );
}
