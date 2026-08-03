import { describe, expect, it } from "vitest";
import { formatPercent } from "../releases/format";
import type { InstallerAsset } from "../releases/types";
import {
  batchProgress,
  fraction,
  installProgress,
  isActive,
  isCancelable,
  isInstallActive,
  isInstallReady,
  stateFraction,
} from "./progress";
import type { AppDownloadState, BatchEntry } from "./types";

function asset(size: number): InstallerAsset {
  return { name: "a.exe", url: "https://github.com/o/r/releases/download/v1/a.exe", size, digest: null };
}

describe("fraction / formatPercent", () => {
  it("is real bytes over total, clamped to [0, 1]", () => {
    expect(fraction(3892, 10000)).toBeCloseTo(0.3892);
    expect(fraction(0, 100)).toBe(0);
    expect(fraction(100, 100)).toBe(1);
    expect(fraction(150, 100)).toBe(1);
    expect(fraction(50, 0)).toBe(0); // no total — never invent progress
  });

  it("formats exactly two decimals (FC-31's precise percent)", () => {
    expect(formatPercent("en", 0.3892)).toBe("38.92%");
    expect(formatPercent("en", 0)).toBe("0.00%");
    // Lands on exactly 100.00% at completion (inputs are clamped by fraction —
    // the single place the [0, 1] invariant lives).
    expect(formatPercent("en", 1)).toBe("100.00%");
    expect(formatPercent("en", 0.9999)).toBe("99.99%");
  });

  it("classifies active and cancelable phases in one place", () => {
    expect(isActive({ phase: "downloading", received: 1, total: 2 })).toBe(true);
    expect(isActive({ phase: "verifying", received: 2, total: 2 })).toBe(true);
    expect(isActive({ phase: "done", path: "x", checksumVerified: true })).toBe(false);
    expect(isActive(undefined)).toBe(false);
    // An orphaned backend download must stay cancelable (post-reload lockout).
    expect(isCancelable({ phase: "failed", code: "alreadyDownloading", received: 0 })).toBe(true);
    expect(isCancelable({ phase: "failed", code: "network", received: 0 })).toBe(false);
    expect(isCancelable({ phase: "downloading", received: 1, total: 2 })).toBe(true);
  });

  it("derives the fraction from each download phase", () => {
    expect(stateFraction(undefined)).toBe(0);
    expect(stateFraction({ phase: "downloading", received: 25, total: 100 })).toBe(0.25);
    expect(stateFraction({ phase: "verifying", received: 100, total: 100 })).toBe(1);
    expect(stateFraction({ phase: "done", path: "x", checksumVerified: true })).toBe(1);
    expect(stateFraction({ phase: "failed", code: "network", received: 25 })).toBe(0);
    expect(stateFraction({ phase: "canceled", received: 25 })).toBe(0);
    // Install phases (FC-41): the bar restarts for the install stage and only
    // reads 1 from a real success.
    expect(stateFraction({ phase: "installing", fraction: 0.6, path: "x", checksumVerified: true })).toBe(0.6);
    expect(stateFraction({ phase: "installing", fraction: 1.7, path: "x", checksumVerified: true })).toBe(1);
    expect(stateFraction({ phase: "waitingInstall", path: "x", checksumVerified: true })).toBe(0);
    expect(stateFraction({ phase: "installed", path: "x", checksumVerified: true })).toBe(1);
    expect(stateFraction({ phase: "installFailed", code: "installerFailed", path: "x", checksumVerified: true })).toBe(0);
  });

  it("separates install activity from download activity (distinct Cancel affordances)", () => {
    const installing: AppDownloadState = { phase: "installing", fraction: 0.5, path: "x", checksumVerified: true };
    const waiting: AppDownloadState = { phase: "waitingInstall", path: "x", checksumVerified: true };
    expect(isInstallActive(installing)).toBe(true);
    expect(isInstallActive(waiting)).toBe(true);
    expect(isInstallActive({ phase: "downloading", received: 1, total: 2 })).toBe(false);
    expect(isActive(installing)).toBe(false);
    expect(isCancelable(installing)).toBe(false);
  });

  it("knows which states an install can (re)start from", () => {
    expect(isInstallReady({ phase: "done", path: "x", checksumVerified: true })).toBe(true);
    expect(isInstallReady({ phase: "installFailed", code: "busy", path: "x", checksumVerified: true })).toBe(true);
    expect(isInstallReady({ phase: "installCanceled", path: "x", checksumVerified: true })).toBe(true);
    expect(isInstallReady({ phase: "installing", fraction: 0.2, path: "x", checksumVerified: true })).toBe(false);
    expect(isInstallReady({ phase: "downloading", received: 1, total: 2 })).toBe(false);
    expect(isInstallReady(undefined)).toBe(false);
  });
});

describe("batchProgress (FC-32)", () => {
  const entries: BatchEntry[] = [
    { appId: "a", asset: asset(100) },
    { appId: "b", asset: asset(300) },
    { appId: "c", asset: asset(600) },
  ];

  function states(map: Record<string, AppDownloadState>): Map<string, AppDownloadState> {
    return new Map(Object.entries(map));
  }

  it("aggregates real bytes across the batch", () => {
    const progress = batchProgress(
      entries,
      states({
        a: { phase: "done", path: "x", checksumVerified: true },
        b: { phase: "downloading", received: 150, total: 300 },
        // c not started yet
      }),
    );
    // (100 + 150 + 0) / (100 + 300 + 600)
    expect(progress.fraction).toBeCloseTo(250 / 1000);
    expect(progress.done).toBe(1);
    expect(progress.failed).toBe(0);
    expect(progress.finished).toBe(false);
  });

  it("a failure isolates: the bar still reaches exactly 100% for the rest", () => {
    const progress = batchProgress(
      entries,
      states({
        a: { phase: "done", path: "x", checksumVerified: true },
        b: { phase: "failed", code: "network", received: 150 },
        c: { phase: "done", path: "y", checksumVerified: true },
      }),
    );
    // The failed entry freezes at its real 150 bytes on both sides.
    expect(progress.fraction).toBe(1);
    expect(progress.done).toBe(2);
    expect(progress.failed).toBe(1);
    expect(progress.finished).toBe(true);
  });

  it("a failure never moves the aggregate bar backward", () => {
    const before = batchProgress(
      entries,
      states({
        a: { phase: "done", path: "x", checksumVerified: true },
        b: { phase: "downloading", received: 270, total: 300 },
        c: { phase: "downloading", received: 60, total: 600 },
      }),
    );
    const after = batchProgress(
      entries,
      states({
        a: { phase: "done", path: "x", checksumVerified: true },
        b: { phase: "failed", code: "network", received: 270 },
        c: { phase: "downloading", received: 60, total: 600 },
      }),
    );
    expect(after.fraction).toBeGreaterThanOrEqual(before.fraction);
  });

  it("cancels are counted separately from failures (honest summary)", () => {
    const progress = batchProgress(entries, states({ a: { phase: "canceled", received: 40 } }));
    expect(progress.failed).toBe(0);
    expect(progress.canceled).toBe(1);
    expect(progress.finished).toBe(false);
  });

  it("an empty batch is never 'finished'", () => {
    expect(batchProgress([], states({})).finished).toBe(false);
  });

  it("an entry that continued into its install stays fully counted (no backward bar)", () => {
    const atDone = batchProgress(
      entries,
      states({
        a: { phase: "done", path: "x", checksumVerified: true },
        b: { phase: "done", path: "y", checksumVerified: true },
        c: { phase: "done", path: "z", checksumVerified: true },
      }),
    );
    const whileInstalling = batchProgress(
      entries,
      states({
        a: { phase: "installing", fraction: 0.3, path: "x", checksumVerified: true },
        b: { phase: "installed", path: "y", checksumVerified: true },
        c: { phase: "installFailed", code: "installerFailed", path: "z", checksumVerified: true },
      }),
    );
    expect(atDone.fraction).toBe(1);
    expect(whileInstalling.fraction).toBe(1);
    expect(whileInstalling.done).toBe(3);
    expect(whileInstalling.finished).toBe(true);
  });
});

describe("installProgress (FC-41)", () => {
  function states(map: Record<string, AppDownloadState>): Map<string, AppDownloadState> {
    return new Map(Object.entries(map));
  }

  it("averages the staged fractions across the install stage", () => {
    const progress = installProgress(
      ["a", "b"],
      states({
        a: { phase: "installing", fraction: 0.5, path: "x", checksumVerified: true },
        b: { phase: "waitingInstall", path: "y", checksumVerified: true },
      }),
    );
    expect(progress.fraction).toBeCloseTo(0.25);
    expect(progress.installed).toBe(0);
  });

  it("lands on exactly 100% and counts outcomes honestly", () => {
    const progress = installProgress(
      ["a", "b", "c"],
      states({
        a: { phase: "installed", path: "x", checksumVerified: true },
        b: { phase: "installFailed", code: "elevationDeclined", path: "y", checksumVerified: true },
        c: { phase: "installCanceled", path: "z", checksumVerified: true },
      }),
    );
    expect(progress.fraction).toBe(1);
    expect(progress.installed).toBe(1);
    expect(progress.failed).toBe(1);
    expect(progress.canceled).toBe(1);
  });

  it("an empty stage reports no progress", () => {
    expect(installProgress([], states({})).fraction).toBe(0);
  });
});
