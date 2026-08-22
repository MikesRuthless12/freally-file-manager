/**
 * Screenshots for `docs/documentation.html`.
 *
 * Every documented step that says "click X" gets a picture with X boxed
 * in red. The shots come from the real UI through the same harness the
 * §4 specs use, so the reader sees the app's actual layout, fonts and
 * controls — not a mockup that goes stale the moment the UI moves.
 *
 * What is mocked is the IPC layer only (see `e2e/README.md`): the paths,
 * sizes and progress numbers on screen are seeded, the way any product
 * screenshot uses staged data.
 *
 * Skipped unless `DOCS_SHOTS=1`, so it never slows the §4 suite:
 *
 *     DOCS_SHOTS=1 pnpm exec playwright test e2e/docs-screenshots.spec.ts --project=chromium
 *
 * Chromium only: Tauri renders in WebView2 on Windows, so Chromium is
 * the engine these pictures should be taken in. Output goes to
 * `docs/img/<section>-<nn>-<slug>.png`, which `documentation.html`
 * references from a collapsed "See screenshot" disclosure per step.
 *
 * Harness convention, the one that bites: `tauri.handles({...})` must be
 * registered AFTER `page.goto("/")`, because navigating rebuilds the
 * mock registry with defaults only.
 */

import { test } from "./fixtures/test";
import { fullSettings } from "./fixtures/settings";
import { FAKE_QR_PNG_BASE64 } from "./fixtures/fake-qr";
import { drawCallouts, clearCallouts, type Callout } from "./fixtures/annotate";
import type { Page, Locator } from "@playwright/test";
import type { TauriFixture } from "./fixtures/test";

const OUT = "../../docs/img";
const enabled = !!process.env.DOCS_SHOTS;
const MIB = 1024 * 1024;

/** Annotate, capture, then strip the overlays so they cannot leak. */
async function shot(page: Page, name: string, callouts: Callout[] = []): Promise<void> {
  await drawCallouts(page, callouts);
  await page.screenshot({ path: `${OUT}/${name}.png` });
  await clearCallouts(page);
}

/**
 * Navigate and wait until the shell has actually rendered.
 *
 * Takes `tauri` although it never calls it. Playwright fixtures are lazy:
 * a test whose signature is `async ({ page })` never instantiates the
 * `tauri` fixture, so the IPC shim and its default handlers are never
 * installed, every `invoke` resolves `undefined`, and the app renders a
 * bare `main` carrying the raw `{window-title}` key — a blank white page
 * with no error anywhere. Requiring the fixture here makes that
 * impossible to forget; the underscore says the value itself is unused.
 *
 * The retry covers the other failure mode: `goto` resolves on load, but
 * the UI only appears once the boot IPC has answered and the translation
 * bundle is in, and on a busy machine that can outlast the first wait.
 */
async function boot(page: Page, _tauri: TauriFixture): Promise<void> {
  const shell = page.getByRole("button", { name: /add folders/i }).first();
  for (let attempt = 1; attempt <= 3; attempt++) {
    if (attempt === 1) await page.goto("/");
    else await page.reload();
    try {
      await shell.waitFor({ state: "visible", timeout: 20_000 });
      return;
    } catch {
      // Intermittently the shell comes up empty — a bare `main` carrying
      // the raw `{window-title}` key, meaning the boot IPC answered
      // `undefined` and every pane rendered nothing. A reload re-runs the
      // init scripts and it comes up correctly. Retrying is the honest fix
      // here: these captures do not gate anything, and a flaky harness
      // boot is not something a screenshot run should have an opinion on.
      if (attempt === 3) throw new Error("app shell never rendered after 3 attempts");
    }
  }
}

/**
 * Serialise a real settings DTO into the page before opening Settings.
 *
 * The fixture's own default is `() => fullSettings()`, and `handles`
 * stringifies a handler body to re-evaluate it in the page — so that
 * closure over a Node-scope import is a `ReferenceError` there, the DTO
 * never arrives, and the modal sits on "Loading settings…" forever with
 * no error surfaced. `handleValue` sends the value itself, which is what
 * `visual-smoke.spec.ts` does for the same reason.
 */
async function seedSettings(tauri: TauriFixture): Promise<void> {
  await tauri.handleValue("get_settings", fullSettings());
  await tauri.handleValue("list_profiles", []);
  await tauri.handles({
    update_settings: (args: Record<string, unknown> | undefined) =>
      (args as { dto: unknown } | undefined)?.dto,
  });
}

/** Open Settings and return the modal, the way visual-smoke does. */
async function openSettings(page: Page): Promise<Locator> {
  await page.getByRole("button", { name: /^settings$/i }).first().click();
  const modal = page.getByRole("dialog").filter({ hasText: /settings/i });
  await modal.waitFor({ state: "visible", timeout: 10_000 });
  await page.waitForTimeout(700);
  return modal;
}

/** Switch Settings tabs and let the pane settle before a capture. */
async function tab(page: Page, modal: Locator, name: RegExp): Promise<void> {
  await modal.getByRole("tab", { name }).click();
  await page.waitForTimeout(600);
}

/** Escape hatch for a browser Playwright did not manage to unpack itself:
 *  point `DOCS_SHOTS_CHROME` at any Chrome/Chromium binary. The pictures
 *  only need a Chromium engine, not the exact pinned build. */
const chrome = process.env.DOCS_SHOTS_CHROME;

// Top-level rather than inside the describe: Playwright refuses
// `launchOptions` in a describe group, because it forces a new worker.
test.use({
  viewport: { width: 1280, height: 800 },
  ...(chrome ? { launchOptions: { executablePath: chrome } } : {}),
});

test.describe("documentation screenshots", () => {
  test.skip(!enabled, "set DOCS_SHOTS=1 to capture documentation screenshots");

  /* ---------------------------------------------------------------- */
  /* Overview + install                                                */
  /* ---------------------------------------------------------------- */

  test("overview — the main window", async ({ page, tauri }) => {
    await boot(page, tauri);
    await page.waitForTimeout(1_200);
    await shot(page, "install-01-first-launch");
  });

  /* ---------------------------------------------------------------- */
  /* Copy                                                              */
  /* ---------------------------------------------------------------- */

  test("copy — add sources through to a running transfer", async ({ page, tauri }) => {
    await boot(page, tauri);
    await tauri.handles({
      path_metadata: () => [{ isDir: false, size: 100 * 1024 * 1024 }],
      destination_free_bytes: () => 1024 * 1024 * 1024 * 1024,
      path_total_bytes: () => 100 * 1024 * 1024,
      enumerate_tree_files: () => ({
        files: [{ path: "/tmp/source/holiday-photos.zip", size: 100 * 1024 * 1024 }],
        overflow: false,
      }),
      "plugin:dialog|open": () => "/tmp/dst",
      start_copy: () => [42],
    });

    await shot(page, "copy-01-add-sources", [
      { target: page.getByRole("button", { name: /add files/i }), step: 1 },
      { target: page.getByRole("button", { name: /add folders/i }), step: 2 },
    ]);

    await tauri.emit("drop-received", { paths: ["/tmp/source/holiday-photos.zip"] });
    const staging = page.getByRole("dialog", { name: /transfer dropped/i });
    await staging.waitFor({ state: "visible", timeout: 5_000 });
    await page.waitForTimeout(400);

    await shot(page, "copy-02-pick-destination", [
      { target: staging.getByRole("button", { name: /pick destination/i }), step: 1 },
    ]);

    await staging.getByRole("button", { name: /pick destination/i }).click();
    await staging.getByText("/tmp/dst").waitFor({ state: "visible", timeout: 5_000 });

    await shot(page, "copy-03-start-copying", [
      { target: staging.getByRole("button", { name: /start copying/i }), step: 1 },
    ]);

    // Same dialog, different control — this is the Move walkthrough's step.
    await shot(page, "move-01-operation-and-collision");

    await staging.getByRole("button", { name: /start copying/i }).click();

    await tauri.emit("job-added", {
      id: 42,
      kind: "copy",
      src: "/tmp/source/holiday-photos.zip",
      dst: "/tmp/dst",
      state: "running",
      bytesDone: 0,
      bytesTotal: 100 * MIB,
      filesDone: 0,
      filesTotal: 1,
      rateBps: 0,
      etaSeconds: null,
      lastError: null,
    });
    await tauri.emit("job-progress", {
      id: 42,
      bytesDone: 62 * MIB,
      bytesTotal: 100 * MIB,
      filesDone: 0,
      filesTotal: 1,
      rateBps: 32 * MIB,
      etaSeconds: 2,
    });
    await page.waitForTimeout(700);

    await shot(page, "copy-04-progress");
    await shot(page, "batch-03-activity-when-done");
  });

  /* ---------------------------------------------------------------- */
  /* Settings — transfer, verify, filters                              */
  /* ---------------------------------------------------------------- */

  test("settings — opening the panel, transfer and filters", async ({ page, tauri }) => {
    await boot(page, tauri);

    await shot(page, "settings-01-open", [
      { target: page.getByRole("button", { name: /^settings$/i }).first(), step: 1 },
    ]);

    await seedSettings(tauri);
    const modal = await openSettings(page);

    await shot(page, "settings-02-tabs", [
      { target: modal.getByRole("tablist").first(), step: 1 },
    ]);

    await tab(page, modal, /^transfer$/i);
    await shot(page, "verify-01-algorithm");

    await tab(page, modal, /^filters$/i);
    await shot(page, "batch-02-filters");
  });

  /* ---------------------------------------------------------------- */
  /* Settings — general, shell, secure delete                          */
  /* ---------------------------------------------------------------- */

  test("settings — hotkey, shell integration and secure delete", async ({ page, tauri }) => {
    await boot(page, tauri);
    await seedSettings(tauri);
    const modal = await openSettings(page);

    await tab(page, modal, /^general$/i);
    await shot(page, "hotkey-01-combo");

    await tab(page, modal, /^shell$/i);
    await shot(page, "shell-01-context-menu");

    await tab(page, modal, /^secure delete$/i);
    await shot(page, "secure-delete-01-default-method");
  });

  /* ---------------------------------------------------------------- */
  /* Settings — updates, server, mobile, remotes                       */
  /* ---------------------------------------------------------------- */

  test("settings — updates, server, mobile and remotes", async ({ page, tauri }) => {
    await boot(page, tauri);
    await tauri.handles({
      updater_check_now: () => ({
        checked: true,
        available: false,
        availableVersion: null,
        notes: null,
        url: null,
      }),
    });
    await seedSettings(tauri);
    const modal = await openSettings(page);

    await tab(page, modal, /^updates$/i);
    await shot(page, "settings-03-updates", [
      { target: modal.getByRole("button", { name: /check for updates/i }), step: 1 },
    ]);

    await tab(page, modal, /^server$/i);
    await shot(page, "server-01-panel");

    await tab(page, modal, /^mobile$/i);
    await shot(page, "share-01-mobile");

  });

  /* ---------------------------------------------------------------- */
  /* Settings — the panes the remaining sections point at              */
  /* ---------------------------------------------------------------- */

  test("settings — merge, sanitize, advanced, profiles and bug report", async ({ page, tauri }) => {
    await boot(page, tauri);
    await seedSettings(tauri);
    const modal = await openSettings(page);

    await tab(page, modal, /^collaboration$/i);
    await shot(page, "merge-01-collaboration");

    await tab(page, modal, /whole-drive|sanitize/i);
    await shot(page, "secure-delete-02-whole-drive-sanitize");

    await tab(page, modal, /^advanced$/i);
    await shot(page, "privacy-01-advanced");

    await tab(page, modal, /^profiles$/i);
    await shot(page, "batch-04-profiles");

    await tab(page, modal, /report a bug/i);
    await shot(page, "troubleshooting-02-report-a-bug");
  });

  /* ---------------------------------------------------------------- */
  /* First-launch mobile onboarding                                    */
  /* ---------------------------------------------------------------- */

  // Nested describe purely to scope the viewport: `test.use` applies to the
  // whole block it sits in, so putting it beside the other tests would
  // resize all of them. This modal is captured at the app's real default
  // window size — its whole problem was height against 640px, and a taller
  // viewport would hide that.
  test.describe("at the default window size", () => {
    test.use({ viewport: { width: 1120, height: 640 } });

    test("share — the first-launch pairing modal", async ({ page, tauri }) => {
    // This modal is decided once, from settings read during `onMount`, so
    // the override has to be in place before the app's own code runs — and
    // it has to be an *init script*, not a `tauri.handleValue` on
    // about:blank: the shim's handler map lives in the page's JS context,
    // which the navigation destroys. Registering last means this
    // `setHandler` wins over the fixture's `setHandlerIfMissing` defaults.
    const s = fullSettings();
    const dto = {
      ...s,
      general: { ...s.general, mobileOnboardingDismissed: false },
      mobile: { ...s.mobile, pairEnabled: false, pairings: [] },
    };
    await page.addInitScript(
      (seed: { dto: unknown; qr: string }) => {
        const reg = window.__freally_e2e__;
        if (!reg) return;
        reg.setHandler("get_settings", () => seed.dto);
        reg.setHandler("mobile_onboarding_qr", () => ({
          url: "http://192.168.1.24:1421/pwa",
          qrPngBase64: seed.qr,
        }));
      },
      { dto, qr: FAKE_QR_PNG_BASE64 },
    );
    void tauri;

    await page.goto("/");
    const modal = page.getByRole("dialog").filter({ hasText: /pair|phone|mobile/i }).first();
    await modal.waitFor({ state: "visible", timeout: 30_000 });
    await page.waitForTimeout(800);

    await shot(page, "share-02-first-launch-pairing");
    });
  });

  /* ---------------------------------------------------------------- */
  /* Footer drawers                                                    */
  /* ---------------------------------------------------------------- */

  test("footer — history, totals, library and the error log", async ({ page, tauri }) => {
    await boot(page, tauri);
    await page.waitForTimeout(900);

    await shot(page, "history-01-footer-entry", [
      { target: page.getByRole("button", { name: /^history$/i }).first(), step: 1 },
      { target: page.getByRole("button", { name: /^totals$/i }).first(), step: 2 },
    ]);

    await shot(page, "library-01-footer-entry", [
      { target: page.getByRole("button", { name: /^library$/i }).first(), step: 1 },
      { target: page.getByRole("button", { name: /^sync$/i }).first(), step: 2 },
    ]);

    await shot(page, "troubleshooting-01-error-log", [
      { target: page.getByRole("button", { name: /^error log$/i }).first(), step: 1 },
    ]);
  });
});
