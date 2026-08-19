import { describe, expect, it } from "vitest";
import { computeStatus, downloadAction } from "./status";

describe("computeStatus", () => {
  it("is not-installed when no version was detected", () => {
    expect(computeStatus(null, "1.0.0")).toBe("not-installed");
    expect(computeStatus(undefined, "1.0.0")).toBe("not-installed");
  });

  it("is update-available only when installed is strictly older than latest", () => {
    expect(computeStatus("1.0.0", "1.1.0")).toBe("update-available");
    expect(computeStatus("0.9.0", "1.0.0")).toBe("update-available");
  });

  it("is installed when up-to-date or ahead of the latest release", () => {
    expect(computeStatus("1.1.0", "1.1.0")).toBe("installed");
    expect(computeStatus("1.2.0", "1.1.0")).toBe("installed"); // local build ahead
  });

  it("never claims update-available when the latest version is unknown", () => {
    expect(computeStatus("1.0.0", null)).toBe("installed");
    expect(computeStatus("1.0.0", undefined)).toBe("installed");
  });
});

describe("downloadAction", () => {
  it("maps each status to its download-button action", () => {
    expect(downloadAction("not-installed")).toBe("install");
    expect(downloadAction("update-available")).toBe("update");
    expect(downloadAction("installed")).toBe("redownload");
  });
});
