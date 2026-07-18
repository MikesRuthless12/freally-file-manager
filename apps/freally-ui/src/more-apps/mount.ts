// The React island for "More Freally apps": mounts the vendored, view-only
// CentralPanel into a Svelte-owned container. Svelte owns show/hide + teardown;
// React owns the panel subtree. The panel is localized through our Fluent i18n
// bridge (its fcp-* catalogs, keyed to File Manager's active locale) and opens
// external links via the Tauri opener. allowDownloads is false — a pure
// showcase with no engine and no download/install controls.
import { createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { CentralPanel, type PanelHost } from "@freally/central-panel";
import { panelT, activeLocale } from "./i18n-bridge";

const HOST: PanelHost = {
  openExternal: async (url: string) => {
    // Defense-in-depth: only ever hand http(s) URLs to the OS opener.
    let parsed: URL;
    try {
      parsed = new URL(url);
    } catch {
      return;
    }
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return;
    const opener = await import("@tauri-apps/plugin-opener");
    await opener.openUrl(url);
  },
};

function panelElement() {
  return createElement(CentralPanel, {
    t: panelT,
    locale: activeLocale(),
    host: HOST,
    allowDownloads: false,
  });
}

/** Create a React root on `el` and render the view-only panel into it. */
export function mountMoreApps(el: HTMLElement): Root {
  const root = createRoot(el);
  root.render(panelElement());
  return root;
}

/** Re-render so the panel picks up a new active locale. */
export function refreshMoreApps(root: Root): void {
  root.render(panelElement());
}
