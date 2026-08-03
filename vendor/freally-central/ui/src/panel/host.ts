// The panel's shell boundary (FC-50): actions only the host app can perform.
// Central passes its opener-plugin implementations; other hosts pass their own.
import { createContext, useContext } from "react";

export interface PanelHost {
  /** Open an external http(s) URL in the OS browser. */
  openExternal: (url: string) => void | Promise<void>;
  /**
   * Reveal a downloaded file in the OS file manager. Optional — when a host
   * has no reveal capability the panel hides its "Show in folder" action.
   */
  revealInFolder?: (path: string) => void | Promise<void>;
}

export const PanelHostContext = createContext<PanelHost | null>(null);

export function useHost(): PanelHost {
  const ctx = useContext(PanelHostContext);
  if (!ctx) throw new Error("Central panel components must render inside <CentralPanel>");
  return ctx;
}

/**
 * True only for absolute http(s) URLs — the single scheme policy for every
 * link the brand opens. Central's own shell reuses this, so the rule lives in
 * exactly one place.
 */
export function isSafeHttpUrl(url: string): boolean {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return false; // not an absolute URL — nothing safe to open
  }
  return parsed.protocol === "http:" || parsed.protocol === "https:";
}

/**
 * The one path panel code hands a URL to the host. Links come from the catalog
 * manifest and the GitHub API; constraining the scheme here means no host and
 * no manifest can ever be tricked into opening a `file:`/other-scheme URL.
 */
export function openExternalUrl(host: PanelHost, url: string): void {
  if (!isSafeHttpUrl(url)) return;
  // A host may still refuse the URL (e.g. an https-only opener given http) —
  // that refusal is the host's call and must not surface as an unhandled
  // rejection here.
  void Promise.resolve(host.openExternal(url)).catch(() => {});
}
