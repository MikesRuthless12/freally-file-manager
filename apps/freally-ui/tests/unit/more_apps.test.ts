// Embed smoke for "More Freally apps": the vendored, view-only CentralPanel
// renders and localizes through the Fluent i18n BRIDGE (File Manager's own
// i18n is Rust-side and can't host the panel's Fluent catalogs). Uses
// createElement (not JSX) so the file stays a .test.ts.

import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { createElement } from "react";
import { render, screen, cleanup, waitFor } from "@testing-library/react";
import { CentralPanel } from "@freally/central-panel";
import { panelT, activeLocale } from "../../src/more-apps/i18n-bridge";

const HOST = { openExternal: () => {} };

describe("More Freally apps — Fluent i18n bridge", () => {
  it("resolves the panel's fcp-* keys keyed to the app locale", () => {
    expect(activeLocale()).toBe("en");
    expect(panelT("fcp-coming-soon")).toBe("Coming soon");
    expect(panelT("fcp-available")).toBe("Available");
    expect(panelT("fcp-refresh")).not.toBe("fcp-refresh");
  });
});

describe("More Freally apps — panel renders (view-only)", () => {
  beforeEach(() => {
    // Offline: hosted catalog + GitHub fetches fail, so the panel falls back to
    // its bundled catalog and hides counts.
    vi.stubGlobal(
      "fetch",
      vi.fn(() => Promise.reject(new Error("offline"))),
    );
  });
  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  it("mounts catalog cards localized via the bridge, with no download control", async () => {
    render(createElement(CentralPanel, { t: panelT, locale: "en", host: HOST, allowDownloads: false }));

    expect(await screen.findByText("Freally Capture")).toBeInTheDocument();
    await waitFor(() =>
      expect(screen.getAllByText(panelT("fcp-coming-soon")).length).toBeGreaterThan(0),
    );
    expect(screen.queryByText(panelT("fcp-install-all"))).toBeNull();
  });
});
