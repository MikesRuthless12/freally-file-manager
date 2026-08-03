import { useT } from "../i18n";
import type { CatalogApp } from "../catalog/types";
import type { ReleaseState } from "../releases/types";
import type { AppDownloadState } from "../downloads/types";
import { ProductCard } from "./ProductCard";

interface CardGridProps {
  apps: CatalogApp[];
  releases: Map<string, ReleaseState>;
  /** Detected installed version per app id (absent key = not probed). */
  installed: Map<string, string | null>;
  /** Download state per app id (absent key = no download this session). */
  downloads: ReadonlyMap<string, AppDownloadState>;
  /** When false (view-only host), cards show an "Available" label, not "Download". */
  allowDownloads: boolean;
  onOpen: (app: CatalogApp) => void;
}

export function CardGrid({ apps, releases, installed, downloads, allowDownloads, onOpen }: CardGridProps) {
  const t = useT();
  if (apps.length === 0) {
    return <p className="grid-empty">{t("fcp-grid-empty")}</p>;
  }
  return (
    <div className="grid">
      {apps.map((app) => (
        <ProductCard
          key={app.id}
          app={app}
          release={releases.get(app.id)}
          installedVersion={installed.get(app.id)}
          download={downloads.get(app.id)}
          allowDownloads={allowDownloads}
          onOpen={onOpen}
        />
      ))}
    </div>
  );
}
