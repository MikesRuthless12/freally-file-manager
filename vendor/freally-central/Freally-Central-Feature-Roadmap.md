# Freally Central — Feature Roadmap

**Freally Central** is the brand's hub: one desktop app (and an embeddable panel that ships
inside every other Freally app) that shows the whole product family in a card grid — big icons,
per-OS downloads, live install/update status, changelogs & "What's New", real download counts,
and a **Download All** button — with a live 0–100 % download/install progress bar and hands-off
(silent) installation, all in 18 languages.

> **House stack:** Tauri v2 shell + React/TypeScript UI + Rust core, matching Freally Capture.
> Reuse the brand conventions: the Fluent i18n system with **18 locales** and a CI parity lint,
> the self-hosted signed updater, the same code-review / security-review Definition-of-Done gates.

---

> **⚠️ DoD amendment — 2026-07-15.** Three standing items apply to **every phase's** Definition of Done:
>
> - **`/simplify` pass** — run **`/simplify` on the full phase diff** in unison with **`/code-review`**
>   and **`/security-review`**, applying its cleanups (reuse over duplication, no speculative
>   abstraction/config, remove orphans the change created) before the phase is done and before any push.
> - **All 18 locale keys updated** — any new or changed UI string is added to **all 18** Fluent locale
>   catalogs (every locale gets the key); `i18n:lint` parity must be green.
> - **Local CI gate before push** — run **`npm run ci:local`** (mirrors the GitHub 3-OS CI: `cargo fmt
>   --check`, `clippy -D warnings`, `cargo test`, `cargo deny`, UI `typecheck`/`lint`/`test`/`i18n:lint`,
>   and Playwright `test:e2e`) and get it **green before any push** — never push on a red local gate.

## Charter invariants (never violated)

1. **Local-first & honest.** No accounts, no telemetry. Every number shown is real — download
   counts come from GitHub's release `download_count` API (which increments on real downloads),
   **never** a seeded or placeholder figure.
2. **The catalog is data, not code.** The product list, versions, dates, features, icons, and
   download URLs live in a **hosted manifest** (`freally-central.json` on GitHub Pages) so the
   grid updates without shipping a new Central build.
3. **Consent for installs.** Silent/unattended installation runs only on an explicit user action
   (a Download/Install or Download-All click); Central never installs anything unprompted, and it
   only ever runs **official, checksum/signature-verified** Freally installers.
4. **Cross-platform parity.** Windows, macOS, Linux — every feature works on all three, or the
   platform-specific caveat is documented.
5. **18-language UI.** Every button, label, status, and the changelog viewer are localized into
   the same 18 locales as the rest of the brand; a CI parity lint proves the catalogs agree.

---

## The product catalog manifest

A single hosted JSON is the source of truth (see `catalog/freally-central.json` for the schema +
an example). Per app it carries: `id`, `name`, `tagline`, `description`, `features[]`,
`icon` (a large PNG/SVG URL), `startedDate`, the GitHub `repo` (owner/name), the site/landing URL,
the changelog URL, and a per-OS **asset match pattern** (so Central can pick the right installer and
read its `download_count`). Central resolves *live* data (latest version + date, per-OS
`download_count`, installers) from the GitHub Releases API against those patterns — the manifest
holds only what the API can't give (marketing copy, icon, start date, OS→asset rules).

---

## Phase closeout — Definition of Done (applies to EVERY phase)

In addition to each phase's own feature checklist below, **no phase ships until this full closeout
passes.** Run it in this order (reviews and all fixes come **before** any push):

1. **`/code-review`** (high effort) on the phase diff — **fix every finding** (with tests).
2. **`/security-review`** on the phase diff — **fix every finding**.
3. **Fix all bugs** — nothing known-broken ships.
4. **Smoke tests** — build and launch the app on **Windows, macOS, and Linux**; confirm the real UI
   renders and the phase's features actually work end-to-end (not just that tests pass).
5. **Format** — `cargo fmt --check` + `prettier --check` clean.
6. **Lint/type/test** — `cargo clippy -D warnings`, `cargo test` (whole workspace), UI
   `typecheck` + `lint` + `test`, and **`i18n:lint`** (18-locale parity) all green.
7. **Playwright e2e** — `test:e2e` green: automated end-to-end coverage of the phase's user flows
   (grid renders, a card's states, the changelog viewer, and — critically — a mocked
   **download → progress 0→100% → install → Installed ✓** run and **Download All**), so the hub is
   verified without a human clicking through it. Runs in CI on all three OSes.
8. **`cargo deny check`** — advisories, bans, licenses, sources all clean.
9. **Docs** — update the README, add a **CHANGELOG entry for the phase/version**, and update the
   site (its `changelog.html` / `index.html` + the real download counter); bump the version in every
   version-bearing file.
10. **Local CI gate** — run **`npm run ci:local`** and get it **green before pushing**. It mirrors the
    GitHub 3-OS CI in one command (`cargo fmt --check`, `clippy -D warnings`, `cargo test`, `cargo deny`,
    UI `typecheck`/`lint`/`test`/`i18n:lint`, Playwright `test:e2e`) so red CI is caught locally first.
11. **Commit & push until CI is green** — the 3-OS matrix must pass on the PR before merge.
12. **Tag / re-tag** the version (`vX.Y.Z`).
13. **Rebuild the release** — signed installers for all three OSes via the tag pipeline.
14. **Publish** — release with `--latest` (updater endpoint resolves) and publish the docs site.

> Mirrors the Freally-brand rule: **finish `/code-review` + `/security-review` and fix ALL findings
> BEFORE pushing** — push only once it's green-ready.

## Phases

> **Every phase is built and tested on Windows, macOS, and Linux.** No feature is "done" until it
> passes on all three (via the Phase 0 CI matrix), or its platform-specific caveat is documented.

### Phase 0 — GitHub repo, CI/CD & cross-platform pipeline
*Stand up the repository and the release machinery before any product code.*

- **FC-00a — Create the GitHub repo** — `MikesRuthless12/freally-central` (private → public at first
  release): `README`, `LICENSE`, `.gitignore`, issue/PR templates, branch protection on `main`, and
  this roadmap + build-prompts guide committed.
- **FC-00b — 3-OS CI matrix** — GitHub Actions building & testing on **Windows, macOS, and Linux**
  runners on every push/PR: Rust (`fmt`, `clippy -D warnings`, `cargo test`) + UI
  (`typecheck`, `lint`, `test`, `i18n:lint`). A red matrix blocks merge.
- **FC-00c — Signed release pipeline** — a version-tag push builds and **signs** the installers for
  all three OSes (Tauri bundler: NSIS/MSI on Windows, `.dmg`/`.app` on macOS, AppImage/`.deb`/`.rpm`
  on Linux), publishes them to a GitHub Release, and writes the signed `latest.json` updater
  endpoint (publish with `--latest`). Mirror Freally Capture's release workflow.
- **FC-00d — GitHub Pages docs site** — a full docs site under `docs/`, **exactly like the other
  Freally sites** (same layout/stylesheet): `index.html` (hero + features + a per-OS **Download**
  section for Windows/macOS/Linux + the **real per-OS GitHub download counter**), `changelog.html`
  (its own changelog — what was added per version), and `documentation.html`. Pages also serves the
  **catalog manifest** (`freally-central.json`) so the hub's data updates without an app release. A
  starter version of these pages is scaffolded now (see `docs/`).
- **FC-00e — Toolchain pinning** — `rust-toolchain.toml`, Node engines, and a `deny.toml`
  (advisories/licenses) so the 3-OS builds are deterministic.

**Definition of Done — Phase 0**
- [ ] The repo exists with branch protection; the CI matrix is **green on Windows, macOS, and Linux**.
- [ ] A test tag produces **signed installers for all three OSes** on the Release page, and the
      updater endpoint resolves.
- [ ] GitHub Pages serves the catalog manifest at a stable URL the app can fetch.

### Phase 1 — The shell & the catalog grid
*Stand up the app and render every product as a card.*

- **FC-01 — Tauri v2 + React + Rust skeleton** — window, dark theme, the brand's fonts/icon.
- **FC-02 — Catalog loader** — fetch + cache `freally-central.json`; ship a bundled copy as the
  offline fallback so the grid always renders.
- **FC-03 — Product-card grid** — big icon, name, tagline; responsive columns like the reference
  hub; a Type/category filter and a search box.
- **FC-04 — App detail view** — description, **cool features** list, **date started**, latest
  **version + date**, links out to the site.
- **FC-05 — i18n foundation** — the Fluent 18-locale system + CI parity lint from day one.
- **FC-06 — First-run EULA gate** — the EULA (see `EULA.md`) pops up on first use and the app does
  not proceed until the user accepts it; acceptance is remembered (a one-time agreement). This
  matches the brand: every Freally app shows its own one-time EULA on first use, so users know the
  Licensor is not responsible for what they do with the apps they download or install.

**Definition of Done — Phase 1**
- [ ] Grid renders every catalog app on all 3 OSes; offline fallback proven (airplane mode).
- [ ] No hard-coded product data in the UI — everything comes from the manifest.
- [ ] i18n parity lint green across 18 locales; no raw string ids in the UI.

### Phase 2 — Live release data & real download counts
*Wire each card to GitHub's real numbers.*

- **FC-10 — GitHub Releases integration** — per app, resolve the latest release, its date, and
  the per-OS installer asset via the manifest's match patterns. Rate-limit-aware + cached.
- **FC-11 — Real download counters** — per-OS **and** total `download_count` per app, and a
  brand-wide grand total. Auto-refreshing; hidden (not faked) when the API is unreachable.
- **FC-12 — Changelog & What's New** — an in-app viewer that pulls each app's `CHANGELOG.md`
  (or `changelog.html`) and renders the running/latest version's notes — **fully localized
  chrome** around the notes.
- **FC-13 — Version metadata** — show "started {date}" and "v{X} · {date}" per app from the
  manifest + API.

**Definition of Done — Phase 2**
- [ ] Every counter shown is a real GitHub number (verified against the repo's Releases page).
- [ ] Changelog viewer renders each app's latest notes; its buttons/labels are in all 18 locales.
- [ ] Graceful, honest empty states on rate-limit/offline (never a placeholder number).

### Phase 3 — Install detection & status badges
*Know what's on the machine.*

- **FC-20 — Installed-version detection** — per OS: Windows registry/uninstall keys + known
  install paths; macOS `/Applications/*.app` `CFBundleShortVersionString`; Linux package
  managers / `~/.local` / AppImage locations. Map to the manifest's app id.
- **FC-21 — Status badges** — **Installed ✓ / Update available / Not installed**, computed by
  comparing the detected version to the latest release (semver-aware).
- **FC-22 — Per-app actions** — the card's primary button reflects status (Install / Update /
  Open / Re-download).

**Definition of Done — Phase 3**
- [ ] Correct badge for each app on all 3 OSes across installed/outdated/absent states.
- [ ] Detection never blocks the UI (runs off-thread; cached with a manual refresh).

### Phase 4 — Downloads, the progress bar & Download All
*Fetch installers with a precise live percent.*

- **FC-30 — Download engine** — stream the right per-OS installer to a temp dir with a
  **precise percent** (e.g. `38.92%`) from bytes-received / content-length; resumable where
  possible; verify size/checksum (and signature when published) before use.
- **FC-31 — Live progress bar** — a bar bound 0 → 100 % to the download, then to the install,
  showing the two-decimal percent. Per-card and an aggregate bar for Download All.
- **FC-32 — Download All** — queue every app's current-OS installer; bounded concurrency; a
  combined progress + per-app sub-progress; cancel/retry.

**Definition of Done — Phase 4**
- [ ] The percent tracks real bytes on all 3 OSes and reaches exactly 100 % on completion.
- [ ] Download All fetches every app; failures isolate (one bad download never stalls the rest).
- [ ] Downloads are verified (size + checksum/signature) before any install step runs.

### Phase 5 — Hands-off (silent) installation
*Install without the user clicking through wizards.*

- **FC-40 — Silent installers per OS** — run each installer unattended, accepting its defaults:
  Windows NSIS `/S` (and MSI `/qn`), macOS `installer -pkg … -target /` or `hdiutil` mount +
  copy for `.dmg`, Linux `dpkg -i`/`rpm -U`/AppImage `chmod +x` + place. Elevate once per batch
  where the OS requires admin (a single consent prompt, not one per app).
- **FC-41 — Install progress** — feed installer progress into the same 0–100 % bar (parse
  installer output where available; otherwise a determinate staged estimate that still ends at
  100 % on success).
- **FC-42 — Post-install** — flip the card to **Installed ✓**, offer **Open**, and (opt-in)
  create shortcuts using the installer's own defaults.

**Definition of Done — Phase 5**
- [ ] A Download-All → install run completes with **zero** wizard clicks where the OS allows it
      (at most one elevation consent), on all 3 OSes.
- [ ] The bar reaches 100 % and the badge flips to Installed for each app; failures are reported
      honestly, never as success.
- [ ] Central only executes verified official installers; a tampered/unsigned file is refused.

### Phase 6 — "Central inside" embeddable panel
*Ship the hub into every other Freally app.*

- **FC-50 — Shared panel package** — extract the grid + detail + download/install flow into a
  reusable module that any Freally app embeds (e.g. under a "More Freally apps" / Help entry),
  reading the same manifest and reusing that app's i18n runtime.
- **FC-51 — Brand rollout** — a checklist + drop-in instructions so Freally Capture,
  Sourcerer, File Manager, (and future apps) all surface Central; **Freally Studio is excluded**
  from the counter/rollout until its own roadmap exists.

**Definition of Done — Phase 6**
- [x] The panel embeds in at least one shipping Freally app with no duplicated business logic.
      *(Freally Capture: Help → More Freally apps — the vendored `ui/src/panel` + the
      `freally-central-engine` crate from one pinned submodule commit; see EMBEDDING.md.)*
- [x] Panel chrome is fully localized via the host app's 18-locale catalogs.
      *(The panel's `fcp-*` catalogs load into the host's Fluent bundles; proven by
      Capture's `MoreApps.test.tsx` asserting a panel string through Capture's `t()`.)*

### Phase 7 — Polish, distribution & auto-update
*Ship Central itself the way the brand ships everything.*

- **FC-60 — Signed installers + self-hosted updater** — the same signed multi-OS bundle +
  `--latest` updater endpoint pattern the other apps use.
- **FC-61 — Accessibility & themes** — keyboard nav, screen-reader labels, light/dark, reduced
  motion for the progress animations.
- **FC-62 — The public site** — a Freally Central landing/download page carrying the same real
  per-OS GitHub download counter used across the brand.

**Definition of Done — Phase 7**
- [ ] Central ships signed for Windows/macOS/Linux and updates itself via the signed endpoint.
- [ ] a11y pass (keyboard + screen reader) and both themes verified; i18n parity green.

---

## Cross-cutting requirements (apply to every phase)

- **18-language i18n** for every button, label, status, tooltip, and the changelog viewer chrome
  (Fluent catalogs + CI parity lint). The changelog *notes themselves* come from each app verbatim.
- **The progress percent is always real** and two-decimal (e.g. `38.92%`), spanning download →
  install, and it reaches exactly 100 % only on genuine completion.
- **Honest numbers only** — real GitHub download counts; no fabricated totals anywhere.
- **Consent & safety** — installs run only on user action, only official verified installers, and
  request elevation at most once per batch.
