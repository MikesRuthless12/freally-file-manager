import { describe, expect, it } from "vitest";

import { isOpenableUrl, parseReleaseNotes } from "../../src/lib/release-notes";

/** The exact `notes` string the v1.0.0 manifest carried. */
const CHANGELOG =
  "https://github.com/MikesRuthless12/freally-file-manager/compare/v0.22.0...v1.0.0";
const REAL_NOTES = Array(4)
  .fill(`**Full Changelog**: ${CHANGELOG}`)
  .join("\n\n");

describe("parseReleaseNotes", () => {
  it("shows a repeated paragraph once", () => {
    // The release workflow regenerated the release body once per build
    // target, so the Updates pane listed the same line four times.
    const segs = parseReleaseNotes(REAL_NOTES);
    const links = segs.filter((s) => s.kind === "link");
    expect(links).toHaveLength(1);
    expect(links[0]).toEqual({ kind: "link", href: CHANGELOG });

    const text = segs
      .filter((s) => s.kind === "text")
      .map((s) => (s.kind === "text" ? s.value : ""))
      .join("");
    expect(text.match(/Full Changelog/g)).toHaveLength(1);
  });

  it("drops the markdown emphasis markers", () => {
    const text = parseReleaseNotes("**Full Changelog**: nope")
      .map((s) => (s.kind === "text" ? s.value : ""))
      .join("");
    expect(text).toContain("Full Changelog");
    expect(text).not.toContain("**");
  });

  it("keeps dots that are part of the URL", () => {
    // `/compare/v0.22.0...v1.0.0` ends in a digit; nothing to trim.
    const [link] = parseReleaseNotes(CHANGELOG).filter((s) => s.kind === "link");
    expect(link).toEqual({ kind: "link", href: CHANGELOG });
  });

  it("leaves a sentence-ending period out of the link", () => {
    const segs = parseReleaseNotes("See https://example.com/notes.");
    const link = segs.find((s) => s.kind === "link");
    expect(link).toEqual({ kind: "link", href: "https://example.com/notes" });
    // The period is still shown, just not part of the href.
    const text = segs
      .filter((s) => s.kind === "text")
      .map((s) => (s.kind === "text" ? s.value : ""))
      .join("");
    expect(text.endsWith(".")).toBe(true);
  });

  it("keeps the surrounding prose in order", () => {
    const segs = parseReleaseNotes("before https://example.com after");
    expect(segs.map((s) => s.kind)).toEqual(["text", "link", "text"]);
    expect(segs[0]).toEqual({ kind: "text", value: "before " });
    expect(segs[2]).toEqual({ kind: "text", value: " after" });
  });

  it("handles several distinct links", () => {
    const segs = parseReleaseNotes("a https://one.example b https://two.example");
    const hrefs = segs.filter((s) => s.kind === "link").map((s) => (s.kind === "link" ? s.href : ""));
    expect(hrefs).toEqual(["https://one.example", "https://two.example"]);
  });

  it("treats empty or blank notes as nothing to show", () => {
    expect(parseReleaseNotes("")).toEqual([]);
    expect(parseReleaseNotes("   \n\n  ")).toEqual([]);
  });

  it("never emits a link for a non-http scheme", () => {
    // These are remote text; they must stay inert prose.
    for (const hostile of [
      "javascript:alert(1)",
      "file:///etc/passwd",
      "data:text/html,<script>alert(1)</script>",
    ]) {
      const segs = parseReleaseNotes(`click ${hostile} now`);
      expect(segs.filter((s) => s.kind === "link")).toHaveLength(0);
    }
  });
});

describe("isOpenableUrl", () => {
  it("allows only http and https", () => {
    expect(isOpenableUrl("https://example.com")).toBe(true);
    expect(isOpenableUrl("http://127.0.0.1:8080/x")).toBe(true);

    expect(isOpenableUrl("javascript:alert(1)")).toBe(false);
    expect(isOpenableUrl("file:///etc/passwd")).toBe(false);
    expect(isOpenableUrl("data:text/html,x")).toBe(false);
    expect(isOpenableUrl("example.com")).toBe(false);
    expect(isOpenableUrl("")).toBe(false);
  });
});
