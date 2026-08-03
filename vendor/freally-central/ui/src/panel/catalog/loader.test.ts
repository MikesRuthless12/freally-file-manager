import { describe, expect, it } from "vitest";
import { bundledCatalog, normalizeCatalog } from "./loader";

describe("normalizeCatalog", () => {
  it("defaults status to available and coerces missing fields", () => {
    const catalog = normalizeCatalog({ apps: [{ id: "a", name: "A" }] });
    expect(catalog.apps).toHaveLength(1);
    expect(catalog.apps[0].status).toBe("available");
    expect(catalog.apps[0].features).toEqual([]);
    expect(catalog.apps[0].tagline).toBe("");
  });

  it("keeps coming-soon status and skips entries without an id", () => {
    const catalog = normalizeCatalog({
      apps: [{ id: "x", status: "coming-soon" }, { $comment: "skip me" }, {}],
    });
    expect(catalog.apps.map((a) => a.id)).toEqual(["x"]);
    expect(catalog.apps[0].status).toBe("coming-soon");
  });

  it("returns empty apps for malformed input", () => {
    expect(normalizeCatalog(null).apps).toEqual([]);
    expect(normalizeCatalog({}).apps).toEqual([]);
    expect(normalizeCatalog({ apps: "nope" }).apps).toEqual([]);
  });

  it("parses the bundled catalog with Freally Capture available", () => {
    const capture = bundledCatalog.apps.find((a) => a.id === "freally-capture");
    expect(capture).toBeDefined();
    expect(capture?.status).toBe("available");
    expect(bundledCatalog.apps.length).toBeGreaterThan(1);
  });
});
