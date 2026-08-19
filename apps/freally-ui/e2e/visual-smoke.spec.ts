/**
 * Roadmap Gate 1 — visual smoke.
 *
 * Not an assertion suite. This renders each panel and writes a PNG so a
 * human (or a reviewing model) can *look* at it. Assertions catch
 * "the element exists"; they do not catch text overflowing its box, a
 * control rendered off-screen, unreadable contrast, or a pane that is
 * technically present but visually broken.
 *
 * Output: `target/visual-smoke/*.png`.
 *
 * Skipped unless `VISUAL_SMOKE=1`, so it never slows the §4 suite:
 *
 *     VISUAL_SMOKE=1 pnpm exec playwright test e2e/visual-smoke.spec.ts
 *
 * Note the harness convention: `tauri.handles({...})` must be registered
 * AFTER `page.goto("/")`, because navigating rebuilds the mock registry
 * with defaults only.
 */

import { test } from "./fixtures/test";
import { fullSettings } from "./fixtures/settings";
import type { Page } from "@playwright/test";
import type { TauriFixture } from "./fixtures/test";

const OUT = "../../target/visual-smoke";
const enabled = !!process.env.VISUAL_SMOKE;

/** Roster + ffmpeg state worth looking at, including an overflow case. */
async function seed(tauri: TauriFixture) {
  // `handleValue` serialises a VALUE into the page. `handles` stringifies
  // the function body and re-evaluates it there, so a closure over a
  // Node-scope import like `fullSettings` becomes a ReferenceError and the
  // pane hangs forever on "Loading settings…".
  await tauri.handleValue("get_settings", fullSettings());
  await tauri.handleValue("list_profiles", []);
  await tauri.handles({
    update_settings: (args: unknown) => (args as { dto: unknown })?.dto,
    collab_roster: () => ({
      initialized: true,
      epoch: 4,
      members: [
        { label: "alice", recipient: "age1alice00000000000000000000000000000000000000000000000" },
        { label: "bob", recipient: "age1bob0000000000000000000000000000000000000000000000000" },
        {
          label: "a-deliberately-very-long-collaborator-label-to-test-overflow",
          recipient: "age1longlabel0000000000000000000000000000000000000000000",
        },
      ],
      revoked: ["carol", "dave"],
    }),
    merge_ffmpeg_prefs_get: () => ({ enabled: true, path: "/usr/bin/ffmpeg" }),
    merge_ffmpeg_status: () => ({
      available: true,
      path: "/usr/bin/ffmpeg",
      version: "ffmpeg version 7.1 Copyright (c) 2000-2024 the FFmpeg developers",
    }),
  });
}

async function openSettings(page: Page) {
  await page.getByRole("button", { name: /settings/i }).first().click();
  const modal = page.getByRole("dialog").filter({ hasText: /settings/i });
  await modal.waitFor({ state: "visible", timeout: 10_000 });
  return modal;
}

test.describe("Gate 1 — visual smoke", () => {
  test.skip(!enabled, "set VISUAL_SMOKE=1 to capture panel screenshots");

  test("main window", async ({ page, tauri }) => {
    await page.goto("/");
    await seed(tauri);
    await page.waitForTimeout(1_200);
    await page.screenshot({ path: `${OUT}/01-main-window.png` });
  });

  test("settings — collaboration + merge (Build 4)", async ({ page, tauri }) => {
    await page.goto("/");
    await seed(tauri);
    const modal = await openSettings(page);
    await modal.getByRole("tab", { name: /collaboration/i }).click();
    await page.waitForTimeout(1_000);
    await modal.screenshot({ path: `${OUT}/02-settings-collab.png` });
  });

  test("settings — general", async ({ page, tauri }) => {
    await page.goto("/");
    await seed(tauri);
    const modal = await openSettings(page);
    await page.waitForTimeout(800);
    await modal.screenshot({ path: `${OUT}/03-settings-general.png` });
  });

  test("settings — narrow viewport (overflow check)", async ({ page, tauri }) => {
    await page.setViewportSize({ width: 900, height: 700 });
    await page.goto("/");
    await seed(tauri);
    const modal = await openSettings(page);
    await modal.getByRole("tab", { name: /collaboration/i }).click();
    await page.waitForTimeout(1_000);
    await page.screenshot({ path: `${OUT}/04-collab-narrow.png` });
  });
});
