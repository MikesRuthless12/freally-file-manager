import { describe, expect, it } from "vitest";
import { compareVersions } from "./semver";

const sign = (n: number): -1 | 0 | 1 => (n < 0 ? -1 : n > 0 ? 1 : 0);

describe("compareVersions", () => {
  it("orders by numeric release parts", () => {
    expect(sign(compareVersions("1.2.3", "1.2.4"))).toBe(-1);
    expect(sign(compareVersions("1.3.0", "1.2.9"))).toBe(1);
    expect(sign(compareVersions("2.0.0", "1.9.9"))).toBe(1);
    expect(sign(compareVersions("1.2.3", "1.2.3"))).toBe(0);
  });

  it("compares numeric parts as numbers, not strings", () => {
    expect(sign(compareVersions("1.10.0", "1.9.0"))).toBe(1);
    expect(sign(compareVersions("0.2.0", "0.10.0"))).toBe(-1);
  });

  it("tolerates a leading v and differing part counts", () => {
    expect(sign(compareVersions("v1.2.0", "1.2.0"))).toBe(0);
    expect(sign(compareVersions("1.2", "1.2.0"))).toBe(0);
    expect(sign(compareVersions("1.2.0.1", "1.2.0"))).toBe(1);
  });

  it("ranks a prerelease below its release (semver §11)", () => {
    expect(sign(compareVersions("1.0.0-beta", "1.0.0"))).toBe(-1);
    expect(sign(compareVersions("1.0.0", "1.0.0-rc.1"))).toBe(1);
  });

  it("orders prerelease identifiers correctly", () => {
    expect(sign(compareVersions("1.0.0-alpha", "1.0.0-beta"))).toBe(-1);
    expect(sign(compareVersions("1.0.0-alpha.1", "1.0.0-alpha"))).toBe(1);
    expect(sign(compareVersions("1.0.0-1", "1.0.0-alpha"))).toBe(-1); // numeric < alphanumeric
    expect(sign(compareVersions("1.0.0-2", "1.0.0-10"))).toBe(-1); // numeric compared as numbers
  });

  it("ignores build metadata", () => {
    expect(sign(compareVersions("1.0.0+build.5", "1.0.0+build.9"))).toBe(0);
  });
});
