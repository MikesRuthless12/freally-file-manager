import { test, expect } from "@playwright/test";
import { acceptEula } from "./helpers";

// Phase 1 user flows, driven headless against the web build.

test("first-run EULA gate blocks until accepted, then the grid renders", async ({ page }) => {
  await page.addInitScript(() => {
    try {
      localStorage.clear();
    } catch {
      /* ignore */
    }
  });
  await page.goto("/");

  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();

  const accept = page.getByRole("button", { name: /accept/i });
  await expect(accept).toBeDisabled();

  // Scroll the license to the end to enable Accept.
  await page.locator(".eula-body").evaluate((el) => {
    el.scrollTop = el.scrollHeight;
    el.dispatchEvent(new Event("scroll", { bubbles: true }));
  });
  await expect(accept).toBeEnabled();
  await accept.click();

  await expect(page.getByRole("heading", { name: "Freally Capture" })).toBeVisible();
});

test("the Type filter narrows the grid", async ({ page }) => {
  await acceptEula(page);
  await page.goto("/");

  await expect(page.getByRole("heading", { name: "Freally Capture" })).toBeVisible();

  // Filter to available only (Capture is available; Vault is coming-soon).
  await page.getByRole("combobox").selectOption("available");
  await expect(page.getByRole("heading", { name: "Freally Capture" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Freally Vault" })).toHaveCount(0);
});

test("opening a card shows its detail view", async ({ page }) => {
  await acceptEula(page);
  await page.goto("/");

  await page.getByRole("button", { name: /Freally Capture/ }).click();
  await expect(page.getByRole("heading", { level: 2, name: "Freally Capture" })).toBeVisible();
  await expect(page.getByRole("button", { name: /back/i })).toBeVisible();
});
