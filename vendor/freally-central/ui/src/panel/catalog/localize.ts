import type { Translate } from "../i18n";
import type { CatalogApp } from "./types";

// Localized tagline for an app: use the i18n key `tagline-<id>` when it exists,
// otherwise fall back to the manifest's (English) tagline — so a newly added
// catalog app still shows its tagline without an app change.
export function appTagline(t: Translate, app: CatalogApp): string {
  const key = `fcp-tagline-${app.id}`;
  const value = t(key);
  return value === key ? app.tagline : value;
}

// Localized description, falling back to the manifest's description.
export function appDescription(t: Translate, app: CatalogApp): string {
  const key = `fcp-desc-${app.id}`;
  const value = t(key);
  return value === key ? app.description : value;
}

// Localized features: each feature by index via `feat-<id>-<n>`, falling back
// to the manifest's (English) feature when no translation key exists.
export function appFeatures(t: Translate, app: CatalogApp): string[] {
  return app.features.map((feature, i) => {
    const key = `fcp-feat-${app.id}-${i + 1}`;
    const value = t(key);
    return value === key ? feature : value;
  });
}
