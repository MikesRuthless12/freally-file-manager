/**
 * §4.3 Verify — Manual UI golden path.
 *
 * Frontend-side coverage:
 *  - Settings → Transfer → Verify dropdown round-trips through
 *    `update_settings`.
 *  - A `verify-failed` ErrorPrompt surfaces the ErrorModal.
 *  - FFM-M09 hash inspector: the `hash-inspect` event seeds + hashes,
 *    and a pasted digest marks each row Identical / Different.
 *
 * The actual hash-matching + partial-removal logic is engine-side
 * (covered by `cargo test -p freally-core --test phase_03_verify`).
 */

import { expect, test } from "./fixtures/test";
import { fullSettings } from "./fixtures/settings";


test.describe("§4.3 Verify", () => {
  test("Settings → Transfer → Verify = blake3 → update_settings round-trip", async ({
    page,
    tauri,
  }) => {
    await page.goto("/");
    await expect(page.getByText(/drop files or folders/i)).toBeVisible();

    await tauri.handleValue("get_settings", fullSettings());
    await tauri.handles({
      update_settings: (args) => args?.dto,
      list_profiles: () => [],
    });

    // Open Settings via the footer button.
    await page.getByRole("button", { name: /settings/i }).first().click();
    const settingsModal = page.getByRole("dialog").filter({ hasText: /settings/i });
    await expect(settingsModal).toBeVisible({ timeout: 5_000 });

    // Switch to Transfer tab.
    await settingsModal.getByRole("tab", { name: /transfer/i }).click();

    // Find the Verify dropdown and pick BLAKE3.
    const verifySelect = settingsModal.locator("select").filter({
      hasText: /BLAKE3/i,
    });
    await verifySelect.selectOption("blake3");

    // The change handler dispatches `update_settings` with the new dto.
    const updateCall = await tauri.waitForCall("update_settings");
    const dto = updateCall.args?.dto as { transfer?: { verify?: string } } | undefined;
    expect(dto?.transfer?.verify).toBe("blake3");
  });

  test("Verify mismatch → ErrorModal opens with err-verify-mismatch", async ({
    page,
    tauri,
  }) => {
    await page.goto("/");
    await expect(page.getByText(/drop files or folders/i)).toBeVisible();

    await tauri.handles({
      resolve_error: () => undefined,
    });

    // Engine surfaces verify-failed via the standard error-raised
    // event. Same surface as a permission-denied or io-other error.
    await tauri.emit("error-raised", {
      id: 7,
      jobId: 1,
      src: "/tmp/src/file.bin",
      dst: "/tmp/dst/file.bin",
      kind: "verify-failed",
      localizedKey: "err-verify-mismatch",
      message: "BLAKE3 hash mismatch — destination differs from source",
      rawOsError: null,
      createdAtMs: Date.now(),
    });

    // ErrorModal uses role="alertdialog".
    const errorModal = page
      .getByRole("alertdialog")
      .filter({ hasText: /verify|mismatch|hash|file\.bin/i });
    await expect(errorModal.first()).toBeVisible({ timeout: 5_000 });
  });

  // FFM-M09 — hash inspector + clipboard compare.
  const MATCH = "a".repeat(64);
  const OTHER = "b".repeat(64);

  test("hash-inspect event seeds the inspector and a pasted digest gives a per-row verdict", async ({
    page,
    tauri,
  }) => {
    await page.goto("/");
    await expect(page.getByText(/drop files or folders/i)).toBeVisible();

    await tauri.handles({
      hash_inspect: () => ({
        algo: "sha256",
        rows: [
          { path: "/tmp/one.bin", hex: MATCH, size: 1024, error: null },
          { path: "/tmp/two.bin", hex: OTHER, size: 2048, error: null },
        ],
        truncated: false,
      }),
      // Mirror of the Rust parser for the bare-digest case the test uses.
      hash_parse_digest: (args) => {
        const text = String(args?.text ?? "").trim().toLowerCase();
        return /^[0-9a-f]{8,}$/.test(text) ? text : null;
      },
    });

    // Explorer's "Hash with Freally File Manager" verb / `freally --hash`.
    await tauri.emit("hash-inspect", { paths: ["/tmp/one.bin", "/tmp/two.bin"] });

    const inspector = page.getByRole("dialog").filter({ hasText: /hash inspector/i });
    await expect(inspector).toBeVisible({ timeout: 5_000 });
    await expect(inspector.getByText("/tmp/one.bin")).toBeVisible();
    await expect(inspector.getByText(MATCH)).toBeVisible();

    // Paste a digest that matches exactly one of the two rows.
    await inspector.getByRole("textbox").fill(MATCH);
    await expect(inspector.getByText(/^identical$/i)).toHaveCount(1);
    await expect(inspector.getByText(/^different$/i)).toHaveCount(1);

    // Non-digest text falls back to the hint, with no verdicts at all.
    await inspector.getByRole("textbox").fill("not a digest");
    await expect(inspector.getByText(/does not look like a digest/i)).toBeVisible();
    await expect(inspector.getByText(/^identical$/i)).toHaveCount(0);
  });
});
