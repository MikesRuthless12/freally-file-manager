import { test, expect } from "@playwright/test";
import { installMocks, PERCENT_RE } from "./helpers";

// Phase 4 flows: the download engine, the live two-decimal percent, and the
// batch with failure isolation — driven headless with the catalog, the GitHub
// API, and the native engine all mocked through the test-only window.__FC_TEST__
// seam (see helpers.ts). In production the engine is the Rust backend streaming
// real bytes; here it is scripted so the bar's behavior is deterministic.

test("a download streams to a verified 100.00% (FC-30/31)", async ({ page }) => {
  await installMocks(page);
  await page.goto("/");

  await page.getByRole("button", { name: /Freally Capture/ }).click();
  await page.getByRole("button", { name: "Download", exact: true }).click();

  // The live percent is always two decimals while streaming…
  const percent = page.locator(".detail-download-percent");
  await expect(percent).toHaveText(PERCENT_RE);
  // …the bar is exposed to assistive tech as a real progressbar (FC-61)…
  await expect(page.getByRole("progressbar")).toBeVisible();
  // …lands on exactly 100.00% (held through verification)…
  await expect(percent).toHaveText("100.00%");
  await expect(page.locator(".detail-download-phase")).toHaveText("Verifying…");
  // …and ends honestly verified.
  await expect(page.locator(".detail-download-note--ok")).toHaveText("Downloaded & verified");
});

test("a download can be canceled and retried", async ({ page }) => {
  await installMocks(page);
  await page.goto("/");

  await page.getByRole("button", { name: /Freally Capture/ }).click();
  await page.getByRole("button", { name: "Download", exact: true }).click();
  await page.getByRole("button", { name: "Cancel", exact: true }).click();

  await expect(page.locator(".detail-download-note")).toHaveText("Download canceled.");
  // Retry runs the download to completion.
  await page.getByRole("button", { name: "Retry", exact: true }).click();
  await expect(page.locator(".detail-download-note--ok")).toHaveText("Downloaded & verified");
});

test("the batch fetches every app; one download failure isolates (FC-32)", async ({ page }) => {
  await installMocks(page, { failDownloadIds: ["freally-vault"] });
  await page.goto("/");

  const installAll = page.getByRole("button", { name: "Download & install all" });
  await expect(installAll).toBeEnabled();
  await installAll.click();

  // The aggregate bar shows a live two-decimal percent while the batch runs.
  await expect(page.locator(".batch-percent")).toHaveText(PERCENT_RE);

  // The batch finishes despite the forced failure, and reports it honestly —
  // the failed download also never reaches the install stage.
  await expect(page.locator(".batch-label")).toHaveText("1 of 3 downloads failed.");

  // The survivors completed their hands-off install (FC-40).
  await expect(page.locator(".status-badge--installed")).toHaveCount(2);
  await page.getByRole("button", { name: /Freally Capture/ }).click();
  await expect(page.locator(".detail-download-note--ok")).toHaveText("Installed ✓");
});
