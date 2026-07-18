// i18n bridge for the vendored Central panel. File Manager's own i18n is
// Rust-side (freally.ftl served over IPC) and its line parser can't handle
// Fluent plurals/selectors, so the panel can't ride it. Instead we parse the
// panel's OWN fcp-* Fluent catalogs here with @fluent/bundle and expose a
// t()/locale the panel consumes — keyed to File Manager's active locale, so the
// panel follows the app's language.
import { FluentBundle, FluentResource } from "@fluent/bundle";
import { get } from "svelte/store";
import { locale } from "../lib/i18n";

// The panel ships one fcp-*.ftl per locale, same 18 codes the app uses.
const FCP_FILES = import.meta.glob<string>(
  "../../../../vendor/freally-central/ui/src/panel/locales/*.ftl",
  { query: "?raw", import: "default", eager: true },
);

const SOURCES = new Map<string, string>();
for (const [path, source] of Object.entries(FCP_FILES)) {
  const m = path.match(/locales\/([^/]+)\.ftl$/);
  if (m) SOURCES.set(m[1], source);
}

const cache = new Map<string, FluentBundle>();

function bundleFor(code: string): FluentBundle {
  const cached = cache.get(code);
  if (cached) return cached;
  const bundle = new FluentBundle([code, "en"], { useIsolating: false });
  const primary = SOURCES.get(code);
  if (primary) bundle.addResource(new FluentResource(primary));
  // Layer English underneath so a missing key falls back to English, not the id.
  if (code !== "en") {
    const en = SOURCES.get("en");
    if (en) bundle.addResource(new FluentResource(en), { allowOverrides: false });
  }
  cache.set(code, bundle);
  return bundle;
}

/** File Manager's currently active locale code (falls back to "en"). */
export function activeLocale(): string {
  return get(locale).code || "en";
}

/** A Fluent-backed t() for the panel's fcp-* keys, keyed to the app locale. */
export function panelT(key: string, args?: Record<string, string | number>): string {
  const bundle = bundleFor(activeLocale());
  const msg = bundle.getMessage(key);
  if (msg?.value) {
    const errs: Error[] = [];
    const out = bundle.formatPattern(msg.value, args, errs);
    if (errs.length === 0) return out;
  }
  return key;
}
