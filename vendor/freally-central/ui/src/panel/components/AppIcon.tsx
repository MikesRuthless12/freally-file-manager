import { useMemo, useState } from "react";
import type { CatalogApp } from "../catalog/types";

interface AppIconProps {
  app: CatalogApp;
  size: number;
}

function monogram(name: string): string {
  const stripped = name.replace(/^Freally\s+/i, "").trim();
  return (stripped || name).charAt(0).toUpperCase() || "F";
}

// The app's own icon is the big card image, at a uniform full-bleed size. We
// prefer the icon bundled with the app (instant, offline, never cache-stale),
// then the manifest's remote icon, then a monogram — so the grid always renders
// something legible.
export function AppIcon({ app, size }: AppIconProps) {
  const sources = useMemo(
    () => [`/app-icons/${app.id}.png`, app.icon].filter((s): s is string => Boolean(s)),
    [app.id, app.icon],
  );
  const [index, setIndex] = useState(0);
  const src = sources[index];

  if (!src) {
    return (
      <div
        className="app-icon app-icon--mono"
        style={{ width: size, height: size, fontSize: Math.round(size * 0.42) }}
        aria-hidden="true"
      >
        {monogram(app.name)}
      </div>
    );
  }

  return (
    <img
      className="app-icon"
      src={src}
      alt=""
      width={size}
      height={size}
      loading="lazy"
      onError={() => setIndex((i) => i + 1)}
    />
  );
}
