# Changelog

All notable changes to Freally Central are documented here. The top section is
embedded into the app and shown in **Settings → What's New**.

## Unreleased

- **View-only embedding mode**: the "Central inside" panel now accepts an `allowDownloads={false}` prop that turns it into a pure showcase — the card grid, detail view, live release data, **real download counts**, and the **changelog viewer** all remain, while every Download / Install / Update / Open / Download-All control is hidden and an available app's card reads "Available" instead of "Download". Hosts that only surface the catalog (apps still in "coming soon") can embed the panel with no download machinery at all — the Rust engine crate becomes optional, so downloads are impossible at the backend rather than merely hidden. No new user-facing strings; 18-locale parity stays green.
- **Housekeeping**: refreshed the Freally Vault card icon; removed the retired Freally Snipper from the embedding docs and roadmap.

## 0.7.0 — Phase 7: polish, distribution & auto-update

- **In-app auto-update now resolves on every OS** (FC-60): the release pipeline publishes one consolidated, signed `latest.json` covering Windows, macOS (Intel + Apple Silicon) and Linux — built by merging each platform's updater fragment in a dedicated release job, instead of four matrix jobs each overwriting a single-OS manifest. **Settings → Check for updates** now resolves, and every update is still verified against the minisign public key baked into the app before it is applied. macOS builds are now ad-hoc signed, so first launch is a right-click → **Open** rather than a hard Gatekeeper block.
- **Accessibility** (FC-61): every dialog moves focus in on open, traps Tab within itself, closes on Escape, and restores focus to whatever opened it; the first-run EULA is fully keyboard-operable (its text region is focusable, and Accept enables once the text is read or is too short to scroll); progress and terminal status changes are announced to screen readers (`role="progressbar"` with a live value, polite live regions for outcomes); and every control shows a visible keyboard focus ring.
- **Reduced motion** (FC-61): with the OS "reduce motion" setting on, the card hover lift and the progress/updater bar animations are neutralized — the percent still updates, it just no longer animates.
- **Themes** (FC-61): light and dark are verified end to end, including native scrollbars and form controls, which now follow the chosen theme (`color-scheme` is driven at runtime).
- **Public download page** (FC-62): the site offers per-OS installer buttons (Windows, macOS Apple Silicon/Intel, Linux) beside the existing real, auto-incrementing per-OS GitHub download counter.
- No new user-facing strings — the accessibility work reuses the existing 18-locale catalogs; i18n parity stays green.

## 0.6.0 — Phase 6: "Central inside" — the embeddable panel

- The whole hub — the card grid, detail view, live release data, install detection, and the verified download → silent-install flow — is now an **embeddable panel** any Freally app can ship under a "More Freally apps" entry, with **no copy of the business logic**: hosts vendor this repo as a git submodule; one pinned commit supplies both the React panel (`ui/src/panel`) and the Rust engine (the new `freally-central-engine` crate). Central itself now runs on exactly that panel and engine.
- **Freally Capture is the first embed** (Help → More Freally apps): the panel renders in Capture's shell, themed by Capture's tokens (`--fcp-*` variable mapping, light/dark follows the host), localized through **Capture's own 18-locale Fluent runtime** — the panel's `fcp-*` catalogs load into the host's bundles and can never collide with host keys.
- Engine commands are now `central_`-prefixed (`central_install_apps`, `central_launch_app`, …) so they can never collide with a host app's commands; the trust gate (verified-download registry, required checksum, baked owner allowlist, install-time re-hash) is unchanged and rides into every host.
- New **EMBEDDING.md**: the drop-in checklist for Capture, Sourcerer, File Manager and future apps (Freally Studio stays excluded until its own roadmap exists).
- Housekeeping: the modal close button now has a proper localized "Close" label; two long-dead i18n keys removed.

## 0.5.0 — Phase 5: hands-off silent install

- **Silent install** (FC-40): one click carries an app — or every app, via **Download & install all** — through download, verification, and an unattended install with the installer's own defaults. Windows runs the NSIS setup with `/S` (per-user, no UAC prompt; `msiexec /qn` as the MSI fallback), macOS mounts the `.dmg` and copies the app into Applications, Linux places the AppImage in `~/Applications` (deb/rpm install through a **single `pkexec` consent for the whole batch**). At most one elevation consent per batch, zero wizard clicks.
- **Only verified official installers ever execute.** Before anything runs, the engine re-checks, independently of the catalog manifest: the file must have a **matching published SHA-256** (no digest → no silent install), its source release must belong to a **trusted owner compiled into the app**, and the on-disk bytes are **re-hashed** at install time — a tampered file is deleted and refused, with the refusal shown honestly.
- **Install progress in the same bar** (FC-41): the card and detail bars continue through the install stage with a determinate staged estimate that only claims **100%** on the installer's real success; the aggregate bar and batch summary cover the install stage too ("N of M installs failed" — never dressed up as success).
- **Post-install** (FC-42): the badge flips to **Installed ✓** (confirmed by a detection re-probe of the installer's own record) and the detail view offers **Open**, which launches the app from what its installer actually registered.
- Cancel stays honest: pending installs cancel; a running installer is never killed mid-run (that could corrupt an install) and reports its real outcome.
- All new UI strings localized across the 18 locales; i18n parity lint stays green.

## 0.4.0 — Phase 4: downloads, the live progress bar & Download All

- A native **download engine**: each app's right-for-this-machine installer streams from its GitHub release to a temp folder with a **precise two-decimal percent** (e.g. `38.92%`) computed from real bytes received against the API-published size — landing on exactly **100.00%** at completion. Interrupted downloads resume where they left off.
- **Verification before use**: the byte count must match the published size exactly, and when GitHub publishes a SHA-256 digest for the asset the streamed hash must match it — a short, oversized, or corrupted download is refused and discarded, never kept as an installer. Only `https://github.com/<owner>/<repo>/releases/download/…` URLs are ever fetched.
- A **live progress bar** on the card and the detail view, bound 0 → 100% to the download and held through verification, with **Cancel** and **Retry** (retry resumes the partial file).
- **Download All**: one click queues every available app's installer for this OS with bounded concurrency, an aggregate progress bar plus per-app bars, **Cancel all**, and honest results — one failed download never stalls the rest, and the summary reports exactly what failed.
- The top-bar **search box was removed** (seven apps don't need one); the Type filter stays.
- All new UI strings localized across the 18 locales; i18n parity lint stays green.

## 0.3.0 — Phase 3: install detection & status badges

- Installed-version detection per OS — Windows uninstall registry keys, macOS `/Applications/*.app` `Info.plist`, and Linux dpkg/rpm packages (with an AppImage-filename fallback) — mapped to each catalog app. Detection runs off the UI thread and is cached with a manual **Refresh**.
- Status badges on every card and the detail view: **Installed ✓ / Update available / Not installed**, computed by a semver-aware comparison of the detected version to the latest release. Honest by construction — a version is shown only when an installer actually wrote it.
- The detail view's primary action now reflects status — **Install / Update / Re-download** — routing to the verified download source. (Hands-off silent install and one-click **Open** arrive with Phase 4–5.)
- The Windows **NSIS** installer now creates a **Desktop shortcut** on install (and removes it on uninstall); the MSI already did.
- New **`npm run ci:local`** gate mirrors the GitHub 3-OS CI (`cargo fmt`/`clippy`/`test`/`deny`, UI `typecheck`/`lint`/`test`/`i18n:lint`, Playwright e2e) so red CI is caught before pushing.
- All new UI strings localized across the 18 locales; i18n parity lint stays green.

## 0.2.0 — Phase 2: live release data & real download counts

- Live GitHub release data per app: the latest version and release date, resolved from the GitHub Releases API against each app's per-OS asset patterns.
- Real download counts — per-OS and total per app, plus a brand-wide grand total — straight from GitHub's `download_count`, never seeded. Counts are hidden (never faked) when the API is offline or rate-limited.
- In-app **changelog / What's New** viewer: the latest release's notes shown verbatim, with fully localized chrome and links to the app's full changelog and GitHub release.
- Rate-limit-friendly caching with a manual and automatic refresh.
- All new UI strings localized across the 18 locales; i18n parity lint stays green.

## 0.1.0 — Phase 1: the shell & the catalog grid

- Tauri v2 + React/TypeScript shell with a standard light/dark theme (follows the OS by default).
- Product-card grid driven by the hosted catalog manifest, with a bundled offline fallback so the grid always renders.
- App detail view (description, features, start date, link to the app's site); a Type filter and a search box.
- 18-language UI (Fluent) with a CI parity + coverage lint.
- First-run EULA gate (accept to continue; remembered).
- Settings with **About**, **What's New**, and a self-hosted **Check for updates** flow (signed updater).
