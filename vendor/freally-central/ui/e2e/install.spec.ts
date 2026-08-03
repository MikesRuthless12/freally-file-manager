import { test, expect } from "@playwright/test";
import { installMocks, PERCENT_RE } from "./helpers";

// Phase 5 (P5.1): the "works without me touching it" guarantee. One click on
// Download & install all must carry every app through download, verification,
// and silent install — the bar advancing 0 → 100% with the two-decimal percent
// and every card ending Installed ✓ — with the download + install engine
// mocked through the same window.__FC_TEST__ seam the unit tests use.

test("Download & install all runs hands-off to Installed ✓ (FC-40/41/42)", async ({ page }) => {
  await installMocks(page);
  await page.goto("/");

  const installAll = page.getByRole("button", { name: "Download & install all" });
  await expect(installAll).toBeEnabled();
  await installAll.click();

  // The aggregate bar advances with a live two-decimal percent (downloads,
  // then the install stage re-uses the same bar — FC-41)…
  const percent = page.locator(".batch-percent");
  await expect(percent).toHaveText(PERCENT_RE);
  // …the install stage is announced while it runs…
  await expect(page.locator(".batch-label")).toHaveText(/Installing/);
  // …the bar lands on exactly 100.00% (held through the final install beat)…
  await expect(percent).toHaveText("100.00%");
  // …and the batch settles honestly with every card flipped to Installed ✓,
  // zero wizard clicks in between.
  await expect(page.locator(".batch-label")).toHaveText("All apps installed.");
  await expect(page.locator(".status-badge--installed")).toHaveCount(3);
});

test("one failed install is reported honestly; the rest still install", async ({ page }) => {
  await installMocks(page, { failInstallIds: ["freally-vault"] });
  await page.goto("/");

  await page.getByRole("button", { name: "Download & install all" }).click();

  // The batch never dresses the failure up as success…
  await expect(page.locator(".batch-label")).toHaveText("1 of 3 installs failed.");
  // …the other two apps completed their installs…
  await expect(page.locator(".status-badge--installed")).toHaveCount(2);
  // …and the failed app's detail states exactly what happened.
  await page.getByRole("button", { name: /Freally Vault/ }).click();
  await expect(page.locator(".detail-download-note--error")).toHaveText(
    "The installer reported an error.",
  );
});
