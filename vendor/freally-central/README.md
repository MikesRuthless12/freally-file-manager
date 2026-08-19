# Freally Central

**One hub for every Freally-brand app.** A desktop launcher/store that lists all Freally
products in one grid — big icons, per-OS downloads, live install/update status, changelogs,
"What's New", real download counts, and a **Download All** button — with a live 0–100 %
download/install progress bar and hands-off (silent) installation.

> Status: **Phase 7 complete — the roadmap is done.** Central now ships the way the rest of
> the brand does: the release pipeline publishes one consolidated, signed `latest.json`
> covering all three OSes, so **in-app auto-update resolves and verifies on Windows, macOS and
> Linux** (FC-60); the UI has a full accessibility pass — focus-trapped dialogs, a
> keyboard-operable first-run agreement, screen-reader announcements, a visible focus ring, and
> a `prefers-reduced-motion` mode — with light/dark verified end to end (FC-61); and the public
> site carries per-OS download buttons beside the real GitHub download counter (FC-62). The
> whole hub also remains an **embeddable "Central inside" panel** (`ui/src/panel` + the
> `crates/freally-central-engine` crate) that any Freally app vendors as a git submodule —
> **Freally Capture ships it first** under Help → **More Freally apps** — so there is one
> implementation, everywhere. See `EMBEDDING.md` for the drop-in guide,
> `Freally-Central-Feature-Roadmap.md` for the plan, and `Build-Prompts-Guide.md` for the
> phase-by-phase build prompts.

## What it is

- A **standalone desktop app** (Tauri v2 + React + Rust — the Freally house stack), *and* a
  small **embeddable panel** ("Central inside") that ships inside every other Freally app so
  the whole family is reachable from anywhere.
- Driven by a **hosted catalog manifest** (`catalog/freally-central.json`, served from GitHub
  Pages) so the product list, versions, and download URLs update **without** shipping a new
  Central build.

## The product cards (per app)

Each card shows: the big app **icon**, name + tagline, a short description, its **cool
features**, the date the project **started**, the **latest version + date**, and a status
badge — **Installed ✓ / Update available / Not installed** — with per-OS **Download** buttons,
a **changelog + What's New** link, and that app's **download count** (per OS + total).

## Headline behaviours

- **Download All** — grabs the right installer for the current OS for every app at once.
- **Live progress** — a bar that animates 0 → 100 % with a precise percent (e.g. `38.92%`)
  across **both** the download and the (silent) install.
- **Hands-off install** — installers run unattended, accepting their defaults, with no user
  clicks where the OS allows it (NSIS `/S`, MSI `/qn`, macOS `installer`/`hdiutil`, Linux
  package managers / AppImage chmod+run).
- **Real download counts** — per-OS numbers come from GitHub's release `download_count` API
  (auto-incrementing on real downloads), never a seeded/placeholder number.
- **18-language UI** — the same Fluent i18n system and 18 locales as the rest of the brand.

## Layout (planned)

```
Freally Central/
├─ README.md                          (this file)
├─ LICENSE                            (proprietary — All Rights Reserved)
├─ EULA.md                            (shown on first use; "not responsible for what you do")
├─ Freally-Central-Feature-Roadmap.md (phased plan + Definition of Done per phase)
├─ Build-Prompts-Guide.md             (the build prompt for each phase)
├─ catalog/
│  └─ freally-central.json            (the product catalog manifest — data model + example)
└─ docs/                              (the GitHub Pages site — matches the other Freally sites)
   ├─ index.html                      (hero + features + per-OS downloads + real download counter)
   ├─ changelog.html                  (its own changelog)
   ├─ documentation.html              (docs hub)
   └─ styles.css                      (shared brand stylesheet)
```

## Development & release

The Tauri v2 + React + Rust app is live (Phases 1–3). Run it locally with `npm install` at the
repo root, then `npm run tauri dev`.

- **CI** — `.github/workflows/ci.yml` runs a 3-OS matrix (Windows, macOS, Linux): Rust
  `fmt`/`clippy`/`test` + `cargo deny`, and UI `typecheck`/`lint`/`test`/`i18n:lint` +
  Playwright `test:e2e`. Branch protection on `main` requires the **CI Success** gate.
- **Local CI gate** — run **`npm run ci:local`** at the repo root before pushing. It mirrors the CI
  above in one command (add `--no-e2e` for a faster inner loop, `--install` to (re)install deps
  first) and exits non-zero on any failure, so red CI is caught locally. Required by the DoD.
- **Pages** — `.github/workflows/pages.yml` publishes `docs/` and serves the catalog manifest:
  - Site — https://mikesruthless12.github.io/freally-central/
  - Manifest — https://mikesruthless12.github.io/freally-central/freally-central.json
- **Releases** — pushing a `v*` tag runs `.github/workflows/release.yml`: a build matrix
  **signs** the Tauri installers for all three OSes and stages a per-platform updater fragment,
  then a release job merges those fragments into one consolidated, signed `latest.json`
  (covering Windows, macOS Intel + Apple Silicon, and Linux) and opens a **draft** GitHub
  Release with every installer plus that updater endpoint attached. A human publishes the draft.

**Required repository secrets** (Settings → Secrets and variables → Actions) before the first
signed release:

| Secret | Purpose |
|--------|---------|
| `TAURI_SIGNING_PRIVATE_KEY` | Updater signing key (from `tauri signer generate`) |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Password for that key |
| `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` | macOS code-signing + notarization (optional until a public macOS release) |

Windows Authenticode signing is configured in `src-tauri/tauri.conf.json` (added in Phase 1).

## License

**Copyright © 2026 Mike Weaver (Havoc Software) — All Rights Reserved.** Freally Central is
proprietary. It **may not be copied, redistributed, reused, or made available to others without the
express written permission of Mike Weaver** (mythodikalone@gmail.com). See [`LICENSE`](LICENSE).
