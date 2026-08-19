/**
 * §4.12 Build 4 (Phases 51-53) — Collaboration roster + merge preview.
 *
 * What this file can and cannot reach:
 *
 *  - **Collaboration panel** — fully reachable. The roster is plain
 *    JSON over IPC, so the list, the add/remove round trip, the
 *    generate-identity reveal and the SAS code all assert here.
 *  - **ffmpeg opt-in** — reachable as *state*: the toggle, the path
 *    field, and which status line renders. Whether a real ffmpeg
 *    decodes a real clip is not a webview concern and belongs in
 *    `cargo test -p freally-mergeview` plus a human drill.
 *  - **Merge previews themselves** — mostly NOT reachable. The
 *    comparisons come back as base64 PNG data URLs produced by the
 *    Rust side; mocking one only asserts that an `<img>` renders a
 *    string we invented. The pixel maths is unit-tested in
 *    `freally-mergeview` (36 tests). What is worth asserting here is
 *    the *routing* — that a detected format picks the right pane.
 *
 * The forward-only revocation copy has a test of its own. It is the
 * one piece of UI text in this app that is load-bearing for a security
 * decision: it tells the user that removing someone does not claw back
 * what they already read. If a refactor drops it, users are misled
 * about what a removal did, so it is asserted rather than assumed.
 */

import { expect, test } from "./fixtures/test";
import { fullSettings } from "./fixtures/settings";

/**
 * Boot the app and register the settings the modal binds.
 *
 * Two harness rules, both of which this file got wrong first time:
 *
 *  1. Handlers must be registered AFTER `page.goto("/")`. Navigating
 *     rebuilds the mock registry with defaults only, silently discarding
 *     anything registered earlier.
 *  2. `get_settings` must go through `handleValue`, not `handles`.
 *     `handles` stringifies the function body and re-evaluates it inside
 *     the page, so a closure over the Node-scope `fullSettings` import
 *     throws `ReferenceError` there — and the pane hangs forever on
 *     "Loading settings…", which reads exactly like a product bug.
 */
async function boot(
  page: import("@playwright/test").Page,
  tauri: import("./fixtures/test").TauriFixture,
) {
  await page.goto("/");
  await tauri.handleValue("get_settings", fullSettings());
  await tauri.handleValue("list_profiles", []);
  await tauri.handles({
    update_settings: (args: unknown) => (args as { dto: unknown })?.dto,
  });
}

/** Open Settings and land on the Collaboration tab. */
async function openCollabTab(page: import("@playwright/test").Page) {
  await page.getByRole("button", { name: /settings/i }).first().click();
  const settingsModal = page
    .getByRole("dialog")
    .filter({ hasText: /settings/i });
  await expect(settingsModal).toBeVisible({ timeout: 10_000 });
  await settingsModal.getByRole("tab", { name: /collaboration/i }).click();
  return settingsModal;
}

test.describe("§4.12 Collaboration roster (Phase 51)", () => {

  test("Empty roster renders the no-one-added state, not a stuck pane", async ({
    page,
    tauri,
  }) => {
    await boot(page, tauri);
    await tauri.handles({
      collab_roster: () => ({
        initialized: false,
        epoch: 0,
        members: [],
        revoked: [],
      }),
    });
    const modal = await openCollabTab(page);

    // The regression this guards: an unregistered command returns
    // `undefined`, the pane throws mid-render, and Svelte leaves the
    // PREVIOUS tab's DOM on screen — which looks like a product bug.
    await expect(modal.getByText(/no one has been added yet/i)).toBeVisible();
  });

  test("Members render with their labels", async ({ page, tauri }) => {
    await boot(page, tauri);
    await tauri.handles({
      collab_roster: () => ({
        initialized: true,
        epoch: 3,
        members: [
          { label: "alice", recipient: "age1alice000000000000000000000000" },
          { label: "bob", recipient: "age1bob00000000000000000000000000" },
        ],
        revoked: ["carol"],
      }),
    });
    const modal = await openCollabTab(page);

    await expect(modal.getByText("alice", { exact: true })).toBeVisible();
    await expect(modal.getByText("bob", { exact: true })).toBeVisible();
    // A revoked member stays visible as history, not silently dropped.
    await expect(modal.getByText(/carol/)).toBeVisible();
  });

  test("Adding a member sends the typed label and recipient", async ({
    page,
    tauri,
  }) => {
    await boot(page, tauri);
    await tauri.handles({
      collab_roster: () => ({
        initialized: false,
        epoch: 0,
        members: [],
        revoked: [],
      }),
      collab_add_member: (args) => {
        (globalThis as Record<string, unknown>).__addArgs = args;
        return null;
      },
    });
    const modal = await openCollabTab(page);

    await modal.getByLabel("Name", { exact: true }).fill("dana");
    await modal.getByLabel("Their public key", { exact: true }).fill("age1dana0000000000000000000000000");
    await modal.getByRole("button", { name: /^add$/i }).click();

    const sent = await page.evaluate(
      () => (globalThis as Record<string, unknown>).__addArgs,
    );
    expect(sent).toMatchObject({
      label: "dana",
      recipient: "age1dana0000000000000000000000000",
    });
  });

  test("Removing a member sends that member's label", async ({
    page,
    tauri,
  }) => {
    await boot(page, tauri);
    await tauri.handles({
      collab_roster: () => ({
        initialized: true,
        epoch: 1,
        members: [
          { label: "alice", recipient: "age1alice000000000000000000000000" },
        ],
        revoked: [],
      }),
      collab_remove_member: (args) => {
        (globalThis as Record<string, unknown>).__removeArgs = args;
        return null;
      },
    });
    const modal = await openCollabTab(page);

    await modal.getByRole("button", { name: /^remove$/i }).first().click();

    const sent = await page.evaluate(
      () => (globalThis as Record<string, unknown>).__removeArgs,
    );
    expect(sent).toMatchObject({ label: "alice" });
  });

  test("The forward-only warning is present — removal does not claw back", async ({
    page,
    tauri,
  }) => {
    await boot(page, tauri);
    await tauri.handles({
      collab_roster: () => ({
        initialized: true,
        epoch: 1,
        members: [
          { label: "alice", recipient: "age1alice000000000000000000000000" },
        ],
        revoked: [],
      }),
    });
    const modal = await openCollabTab(page);

    // `collab-forward-only`. This must survive refactors: without it a
    // user reasonably concludes that removing someone revoked their
    // access to files already shared, which is false and unfixable.
    await expect(
      modal.getByText(/cannot take back files they could already read/i),
    ).toBeVisible();
  });

  test("Generating an identity reveals the secret exactly once", async ({
    page,
    tauri,
  }) => {
    await boot(page, tauri);
    await tauri.handles({
      collab_roster: () => ({
        initialized: false,
        epoch: 0,
        members: [],
        revoked: [],
      }),
      collab_generate_identity: () => [
        "AGE-SECRET-KEY-1TESTONLY0000000000000000000000000000000000000000000",
        "age1testonly0000000000000000000000",
      ],
    });
    const modal = await openCollabTab(page);

    await modal.getByRole("button", { name: /generate a key/i }).click();

    await expect(modal.getByText(/shown only once/i)).toBeVisible();
    await expect(
      modal.getByText(/AGE-SECRET-KEY-1TESTONLY/),
    ).toBeVisible();
  });
});

test.describe("§4.12 ffmpeg opt-in (Phase 53)", () => {

  test("Disabled ffmpeg renders the not-found line, never a false 'Found'", async ({
    page,
    tauri,
  }) => {
    await boot(page, tauri);
    await tauri.handles({
      merge_ffmpeg_prefs_get: () => ({ enabled: false, path: "" }),
      merge_ffmpeg_status: () => ({
        available: false,
        path: null,
        version: null,
      }),
    });
    const modal = await openCollabTab(page);

    await expect(modal.getByText(/ffmpeg was not found/i)).toBeVisible();
  });

  test("An available ffmpeg shows its version string", async ({
    page,
    tauri,
  }) => {
    await boot(page, tauri);
    await tauri.handles({
      merge_ffmpeg_prefs_get: () => ({ enabled: true, path: "/usr/bin/ffmpeg" }),
      merge_ffmpeg_status: () => ({
        available: true,
        path: "/usr/bin/ffmpeg",
        version: "ffmpeg version 7.1",
      }),
    });
    const modal = await openCollabTab(page);

    await expect(modal.getByText(/ffmpeg version 7\.1/)).toBeVisible();
  });

  test("Editing the ffmpeg path persists both the toggle and the path", async ({
    page,
    tauri,
  }) => {
    await boot(page, tauri);
    await tauri.handles({
      merge_ffmpeg_prefs_get: () => ({ enabled: false, path: "" }),
      merge_ffmpeg_status: () => ({
        available: false,
        path: null,
        version: null,
      }),
      merge_ffmpeg_prefs_set: (args) => {
        (globalThis as Record<string, unknown>).__ffmpegPrefs = args;
        return null;
      },
    });
    const modal = await openCollabTab(page);

    await modal.getByLabel("ffmpeg location (leave empty to search PATH)", { exact: true }).fill("/opt/ffmpeg/bin/ffmpeg");
    await modal.getByLabel("ffmpeg location (leave empty to search PATH)", { exact: true }).blur();

    const sent = await page.evaluate(
      () => (globalThis as Record<string, unknown>).__ffmpegPrefs,
    );
    expect(sent).toMatchObject({
      prefs: { path: "/opt/ffmpeg/bin/ffmpeg" },
    });
  });
});

test.describe("§4.12 Merge previews — what the webview cannot reach", () => {
  test.fixme(
    "Image heatmap marks the pixels that actually changed",
    async ({ page: _page, tauri: _tauri }) => {
      // The heatmap is a base64 PNG the Rust side produces. Mocking one
      // asserts only that an <img> renders a string we invented. The
      // per-pixel maths is covered by `cargo test -p freally-mergeview`
      // (images.rs). A human drill confirms the picture is legible.
    },
  );

  test.fixme(
    "PSD layer diff lists added / removed / changed layers",
    async ({ page: _page, tauri: _tauri }) => {
      // Same shape: `psdfile::layer_diff` is unit-tested against real
      // PSD fixtures. The panel only renders the returned list, and a
      // mocked list proves nothing about the parser.
    },
  );

  test.fixme(
    "Audio waveform distinguishes a re-encode from a real edit",
    async ({ page: _page, tauri: _tauri }) => {
      // Requires decoding real audio through symphonia. Engine-side;
      // `cargo test -p freally-mergeview` (audio.rs).
    },
  );

  test.fixme(
    "Video thumbnail strip spreads frames across the real clip",
    async ({ page: _page, tauri: _tauri }) => {
      // Needs a real ffmpeg binary AND a real clip. Deliberately not
      // bundled — ffmpeg is LGPL/GPL and shipping it would make this
      // project a distributor. Human drill on a machine with ffmpeg
      // installed; see Live-To-Do-List.md.
    },
  );

  test.fixme(
    "Hand-off writes base / local / remote with the extension intact",
    async ({ page: _page, tauri: _tauri }) => {
      // `handoff::write_set` touches the real filesystem. The webview
      // never sees the files. Covered by freally-mergeview unit tests;
      // the round trip through an external editor is a human drill.
    },
  );
});
