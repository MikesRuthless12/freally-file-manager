import { describe, expect, it } from "vitest";
import type { InstallerAsset } from "../releases/types";
import { pickInstaller } from "./pickInstaller";

function asset(name: string): InstallerAsset {
  return { name, url: `https://github.com/o/r/releases/download/v1/${name}`, size: 10, digest: null };
}

describe("pickInstaller", () => {
  it("prefers the NSIS setup.exe over the MSI on Windows", () => {
    const assets = [asset("App_1.0.0_x64_en-US.msi"), asset("App_1.0.0_x64-setup.exe")];
    expect(pickInstaller(assets, "windows", "x86_64")?.name).toBe("App_1.0.0_x64-setup.exe");
  });

  it("falls back to the MSI when no exe ships", () => {
    const assets = [asset("App_1.0.0_x64_en-US.msi")];
    expect(pickInstaller(assets, "windows", "x86_64")?.name).toBe("App_1.0.0_x64_en-US.msi");
  });

  it("matches this machine's architecture on macOS", () => {
    const assets = [asset("App_1.0.0_aarch64.dmg"), asset("App_1.0.0_x64.dmg")];
    expect(pickInstaller(assets, "macos", "aarch64")?.name).toBe("App_1.0.0_aarch64.dmg");
    expect(pickInstaller(assets, "macos", "x86_64")?.name).toBe("App_1.0.0_x64.dmg");
  });

  it("treats a name with no arch token as universal", () => {
    const assets = [asset("App_1.0.0.dmg")];
    expect(pickInstaller(assets, "macos", "aarch64")?.name).toBe("App_1.0.0.dmg");
  });

  it("never picks a foreign-arch asset", () => {
    const assets = [asset("App_1.0.0_aarch64.dmg")];
    expect(pickInstaller(assets, "macos", "x86_64")).toBeNull();
  });

  it("prefers the AppImage on Linux, then .deb, then .rpm", () => {
    const all = [asset("app_1.0.0_amd64.deb"), asset("app-1.0.0-1.x86_64.rpm"), asset("app_1.0.0_amd64.AppImage")];
    expect(pickInstaller(all, "linux", "x86_64")?.name).toBe("app_1.0.0_amd64.AppImage");
    expect(pickInstaller(all.slice(0, 2), "linux", "x86_64")?.name).toBe("app_1.0.0_amd64.deb");
    expect(pickInstaller([all[1]], "linux", "x86_64")?.name).toBe("app-1.0.0-1.x86_64.rpm");
  });

  it("the x86 token never fires inside x86_64 names", () => {
    // An i686-only build must not be offered to an x86_64-incompatible pick,
    // and an x86_64 name must not read as 32-bit x86.
    const assets = [asset("app_1.0.0_i686-setup.exe")];
    expect(pickInstaller(assets, "windows", "x86_64")).toBeNull();
    expect(pickInstaller([asset("app-1.0.0-1.x86_64.rpm")], "linux", "x86")).toBeNull();
  });

  it("returns null for empty or missing asset lists", () => {
    expect(pickInstaller(undefined, "windows", "x86_64")).toBeNull();
    expect(pickInstaller([], "windows", "x86_64")).toBeNull();
  });
});
