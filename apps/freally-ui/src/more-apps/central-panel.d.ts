// Type surface for the vendored React panel imported via the Vite alias
// `@freally/central-panel` (built from vendor/freally-central/ui/src/panel).
// svelte-check type-checks against THIS declaration, not the vendored .tsx
// source — Vite/esbuild transforms the real source at build time. That keeps
// this Svelte project's typecheck decoupled from the panel's React internals.
declare module "@freally/central-panel" {
  import type { FC } from "react";

  export interface PanelHost {
    /** Open an external http(s) URL in the OS browser. */
    openExternal: (url: string) => void | Promise<void>;
    /** Reveal a file in the OS file manager (optional; omitted here). */
    revealInFolder?: (path: string) => void | Promise<void>;
  }

  export type TranslateArgs = Record<string, string | number>;
  export type Translate = (id: string, args?: TranslateArgs) => string;

  export interface CentralPanelProps {
    /** Host translate; must resolve the panel's fcp-* keys. */
    t: Translate;
    /** Host active locale (BCP 47). */
    locale: string;
    /** Shell actions only the host can perform. */
    host: PanelHost;
    /** false → view-only showcase: no download/install controls. */
    allowDownloads?: boolean;
  }

  export const CentralPanel: FC<CentralPanelProps>;
}
