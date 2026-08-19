# "Central inside" — embedding the panel in a Freally app (FC-50/51)

Every Freally desktop app can ship Freally Central's catalog panel — the card
grid, detail view, live release data, install detection, and the verified
download → silent-install flow — under a **"More Freally apps"** menu entry,
with **no copy of the business logic**. The host consumes this repo as a git
submodule: one pinned commit supplies both the React panel
(`ui/src/panel`) and the Rust engine (`crates/freally-central-engine`).

Freally Capture is the reference embed. **Freally Studio is excluded** from
the rollout (and from the brand-wide counter) until its own roadmap exists.

## What the host must have

- Tauri v2 + React 19 + a Fluent i18n runtime whose `t(key, args?)` takes
  `Record<string, string | number>` args (every Freally app's does).
- `resolveJsonModule: true` in the UI tsconfig (the panel bundles its
  fallback catalog as JSON).
- An external-link opener that only opens `http(s)` URLs.

## Drop-in steps

1. **Submodule** (repo root):

   ```sh
   git submodule add https://github.com/MikesRuthless12/freally-central vendor/freally-central
   ```

   CI: every `actions/checkout` step needs `submodules: true`.

2. **Rust engine** — `src-tauri/Cargo.toml`:

   ```toml
   freally-central-engine = { path = "../vendor/freally-central/crates/freally-central-engine" }
   ```

   In the builder setup: `builder = freally_central_engine::attach(builder);`
   and add to `generate_handler![…]`:

   ```rust
   freally_central_engine::detect::central_detect_installed,
   freally_central_engine::download::central_start_download,
   freally_central_engine::download::central_cancel_download,
   freally_central_engine::install::central_install_apps,
   freally_central_engine::install::central_cancel_installs,
   freally_central_engine::install::central_launch_app,
   freally_central_engine::central_platform,
   ```

   All commands carry a `central_` prefix, so they can never collide with the
   host's own. No new capability entries are needed (plain commands aren't
   ACL-gated); the engine's trust gate (verified-download registry, required
   checksum, baked owner allowlist, install-time re-hash) rides along as-is.

3. **Panel alias** — `vite.config.ts`:

   ```ts
   resolve: {
     alias: {
       "@freally/central-panel": fileURLToPath(
         new URL("../vendor/freally-central/ui/src/panel", import.meta.url),
       ),
     },
   },
   ```

   and the matching `paths` entry in the UI tsconfig.

4. **i18n** — load the panel's 18 `fcp-*` catalogs into the host's Fluent
   bundles alongside its own (they can never collide — every key is
   `fcp-`-prefixed). With `import.meta.glob`:

   ```ts
   const PANEL_SOURCES = import.meta.glob(
     "../../../vendor/freally-central/ui/src/panel/locales/*.ftl",
     { query: "?raw", import: "default", eager: true },
   );
   ```

   Layer each locale's panel resource into the same bundle as the host's
   catalog (English first, then the target with `allowOverrides`).

5. **The dialog** — render the panel inside the host's modal shell:

   ```tsx
   import { CentralPanel, type PanelHost } from "@freally/central-panel";

   const HOST: PanelHost = { openExternal }; // http(s)-only opener
   // optional: revealInFolder — omit it and the panel hides "Show in folder"

   <CentralPanel t={t} locale={locale} host={HOST} />
   ```

   Add a "More Freally apps" entry in the Help (or equivalent) menu that
   opens it. The two host-side strings (menu label + dialog title) are the
   host's own keys, localized in its 18 catalogs.

   **Escape contract:** the panel's own modals (the changelog viewer) claim
   Escape in the capture phase and call `preventDefault()`. A host dialog
   wrapping the panel must ignore Escape events with `defaultPrevented`, so
   one keypress closes only the topmost layer.

6. **CSP** — the panel fetches release data and the hosted manifest from the
   webview; downloads/installs run in Rust. Add exactly:
   - `connect-src`: `https://api.github.com https://mikesruthless12.github.io`
   - `img-src`: `https://mikesruthless12.github.io`

7. **Icons** — copy `ui/public/app-icons/` into the host's public dir so
   cards render offline; remote/monogram fallbacks cover the rest.

8. **Theme** — the panel is styled by `ui/src/panel/panel.css`, scoped under
   `.fcp-root` and themed via `--fcp-*` variables (dark defaults built in).
   Map them onto the host's tokens, e.g. Capture:

   ```css
   :root {
     --fcp-bg: var(--color-havoc-bg);
     --fcp-panel: var(--color-havoc-panel);
     --fcp-card: var(--color-havoc-panel);
     --fcp-text: var(--color-havoc-text);
     --fcp-muted: var(--color-havoc-muted);
     --fcp-accent-1: var(--color-havoc-accent);
     --fcp-accent-2: var(--color-havoc-accent-2);
     --fcp-accent: var(--color-havoc-accent);
     --fcp-border: color-mix(in srgb, var(--color-havoc-text) 12%, transparent);
     --fcp-border-strong: color-mix(in srgb, var(--color-havoc-text) 20%, transparent);
   }
   ```

   Also themable: `--fcp-card-hover`, `--fcp-on-accent`, `--fcp-ok`,
   `--fcp-warn`, `--fcp-err`, `--fcp-shadow`, `--fcp-radius`.

9. **Smoke test** — assert the dialog renders a catalog card AND one `fcp-*`
   string (e.g. "Download & install all") through the HOST's `t()`; that
   proves the submodule catalogs are really wired in (see Capture's
   `ui/src/panels/MoreApps.test.tsx`).

## View-only (no-download) hosts

An app that only wants to *showcase* the family — not ship the download/install
machinery — renders the panel with `allowDownloads={false}`:

```tsx
<CentralPanel t={t} locale={locale} host={HOST} allowDownloads={false} />
```

Every Download / Install / Update / Open / Download-All control disappears; the
cards, detail view, live release data, **real download counts**, and the
**changelog viewer** all stay, and an available app's card reads "Available"
instead of "Download". No new strings — the mode reuses the panel's existing
`fcp-*` catalogs, so 18-locale parity is unchanged.

Because nothing can start a download or install, a view-only host can **skip the
Rust engine entirely** (step 2). The panel probes the backend on mount and
degrades gracefully when the `central_*` commands aren't registered, so downloads
become impossible at the backend, not merely hidden; the only thing lost is the
"Installed ✓" detection badge (it needs the engine). That reduces a view-only
embed to a **pure-frontend** drop-in — submodule + panel alias + i18n + dialog +
CSP + icons + theme — with **no `Cargo.toml` / `main.rs` changes**. Steps 2 (Rust
engine) and the engine parts of the CSP are then unnecessary; everything else is
identical.

## Updating a host

```sh
cd vendor/freally-central && git fetch && git checkout <new main commit>
cd ../.. && git add vendor/freally-central && git commit
```

Always pin `main` commits (squash-merged), never feature-branch commits —
those SHAs go away when the branch is deleted.

## Rollout checklist (FC-51)

| App | Status |
|---|---|
| Freally Capture | ✅ embedded — full (download-capable) reference implementation |
| Freally Sourcerer | ☐ pending — view-only |
| Freally File Manager | ☐ pending — view-only |
| Freally AV | ☐ pending — view-only |
| Freally Studio | ☐ pending — view-only (previously deferred; owner opted it in) |
| Freally Vault | — no host UI yet (Rust-only skeleton); appears as a catalog card |
| Freally Player | — no host UI yet (Rust-only skeleton); appears as a catalog card |

Non-React hosts (Rust-only apps like Vault and Player today) have no webview to
embed the React panel into — they are still represented *inside* every embedded
panel as catalog cards (icon + status + info). Once such an app grows a React 19
webview it can take the panel; until then only the engine crate is reusable.
