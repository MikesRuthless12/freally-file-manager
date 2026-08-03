import { test, expect } from "@playwright/test";
import { acceptEula } from "./helpers";

// Phase 2 flows: live release data & real download counts, driven headless with
// the GitHub Releases API mocked so the counts are deterministic.

const RELEASE = {
  tag_name: "v1.4.0",
  name: "1.4.0",
  published_at: "2026-07-02T10:00:00Z",
  html_url: "https://github.com/MikesRuthless12/freally-capture/releases/tag/v1.4.0",
  body: "Phase 2 highlights:\n- Live release data\n- Real download counts",
  assets: [
    {
      name: "Freally-Capture_1.4.0_x64-setup.exe",
      browser_download_url: "https://example.test/setup.exe",
      download_count: 1234,
    },
    {
      name: "Freally-Capture_1.4.0.dmg",
      browser_download_url: "https://example.test/app.dmg",
      download_count: 567,
    },
    {
      name: "freally-capture_1.4.0_amd64.AppImage",
      browser_download_url: "https://example.test/app.AppImage",
      download_count: 89,
    },
  ],
};
// 1234 + 567 + 89 = 1890 total downloads.

test("live version, real download counts, and the changelog render", async ({ page }) => {
  await acceptEula(page);
  // Capture resolves to a release; every other repo has no releases (404).
  await page.route("https://api.github.com/repos/**/releases/latest", async (route) => {
    if (route.request().url().includes("/freally-capture/")) {
      await route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(RELEASE) });
    } else {
      await route.fulfill({ status: 404, contentType: "application/json", body: "{}" });
    }
  });
  await page.goto("/");

  await expect(page.getByRole("heading", { name: "Freally Capture" })).toBeVisible();

  // Brand-wide grand total (Studio excluded, but Studio has no release anyway).
  await expect(page.getByText(/Freally apps downloaded 1,890 times/)).toBeVisible();

  // Card shows the live version + total downloads.
  await expect(page.locator(".card-meta").first()).toContainText("v1.4.0");
  await expect(page.locator(".card-meta").first()).toContainText("1,890 downloads");

  // Detail view: version + per-OS + total counts.
  await page.getByRole("button", { name: /Freally Capture/ }).click();
  await expect(page.getByText("v1.4.0", { exact: true })).toBeVisible();
  await expect(page.getByText("1,234", { exact: true })).toBeVisible(); // Windows
  await expect(page.getByText("567", { exact: true })).toBeVisible(); // macOS
  await expect(page.getByText("89", { exact: true })).toBeVisible(); // Linux
  await expect(page.getByText("1,890", { exact: true })).toBeVisible(); // Total

  // Changelog / What's New viewer shows the notes verbatim.
  await page.getByRole("button", { name: /What's New/ }).click();
  await expect(page.getByText("What's new in Freally Capture")).toBeVisible();
  await expect(page.getByText(/Live release data/)).toBeVisible();
});

test("counts are hidden (honest empty state) when the API is unreachable", async ({ page }) => {
  await acceptEula(page);
  await page.route("https://api.github.com/**", (route) => route.abort());
  await page.goto("/");

  await expect(page.getByRole("heading", { name: "Freally Capture" })).toBeVisible();

  // Open Capture's detail and wait for the honest "unavailable" note (the settle
  // signal), then assert no download figures leaked anywhere.
  await page.getByRole("button", { name: /Freally Capture/ }).click();
  await expect(page.getByText(/unavailable right now/i)).toBeVisible();
  await expect(page.locator(".detail-downloads")).toHaveCount(0);

  await page.getByRole("button", { name: /back/i }).click();
  await expect(page.locator(".card-meta")).toHaveCount(0);
  await expect(page.locator(".brand-total-count")).toHaveCount(0);
});
