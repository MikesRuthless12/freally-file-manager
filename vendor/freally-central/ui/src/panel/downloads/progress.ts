// Percent math and shared state predicates for the live progress bars (FC-31).
// The fraction is always real-bytes / published-size, clamped HERE and only
// here — every value the bars and percent labels render funnels through
// fraction(), so this is the single place the [0, 1] invariant lives.
// (Locale formatting is formatPercent in releases/format.ts, cached per locale.)

import type { AppDownloadState, BatchEntry } from "./types";

const clamp01 = (x: number): number => Math.min(Math.max(x, 0), 1);

/** Bytes-received / total as a fraction in [0, 1]. */
export function fraction(received: number, total: number): number {
  if (!(total > 0)) return 0;
  return clamp01(received / total);
}

/** True while bytes are moving or verification runs — the phases that render a
 *  live DOWNLOAD bar (and where the download Cancel applies). */
export function isActive(state: AppDownloadState | undefined): boolean {
  return state?.phase === "downloading" || state?.phase === "verifying";
}

/** True while the app is queued for or running its silent install (FC-41) —
 *  the phases that render a live INSTALL bar. Distinct from isActive because
 *  the download Cancel affordance must not apply here. */
export function isInstallActive(state: AppDownloadState | undefined): boolean {
  return state?.phase === "waitingInstall" || state?.phase === "installing";
}

/** True when Cancel is meaningful: an active transfer, or an orphaned backend
 *  download ("alreadyDownloading" — e.g. after a reload, where the backend
 *  registry survived but this session's state didn't). */
export function isCancelable(state: AppDownloadState | undefined): boolean {
  return isActive(state) || (state?.phase === "failed" && state.code === "alreadyDownloading");
}

/** True when a verified download is on disk and a silent install can (re)start
 *  from it: the download finished, or a previous install failed/was canceled.
 *  A type guard, so callers can read the carried file (`state.path`) directly. */
export function isInstallReady(
  state: AppDownloadState | undefined,
): state is Extract<
  AppDownloadState,
  { phase: "done" | "installFailed" | "installCanceled" }
> {
  return (
    state?.phase === "done" ||
    state?.phase === "installFailed" ||
    state?.phase === "installCanceled"
  );
}

/** The fraction for one app's live bar: real bytes while downloading, the
 *  staged install estimate while installing (a fresh 0→1 for the install
 *  stage — the label says which stage the bar is showing). */
export function stateFraction(state: AppDownloadState | undefined): number {
  switch (state?.phase) {
    case "downloading":
    case "verifying":
      return fraction(state.received, state.total);
    case "done":
    case "installed":
      return 1;
    case "installing":
      return clamp01(state.fraction);
    default:
      return 0;
  }
}

export interface BatchProgress {
  /** Aggregate fraction across the batch (real bytes; see batchProgress). */
  fraction: number;
  /** Entries that reached "done". */
  done: number;
  /** Entries that failed (canceled ones are counted separately — honest). */
  failed: number;
  /** Entries the user canceled. */
  canceled: number;
  /** True once every entry is terminal. */
  finished: boolean;
}

/**
 * Aggregate Download All progress (FC-32): total real bytes received across the
 * batch over the total expected bytes. A failed or canceled entry freezes at
 * the bytes it actually transferred (they stay on BOTH sides of the fraction),
 * so the bar never moves backward and still lands on exactly 100% when the
 * successful set finishes — one bad download never wedges the aggregate.
 */
export function batchProgress(
  entries: BatchEntry[],
  states: ReadonlyMap<string, AppDownloadState>,
): BatchProgress {
  let receivedSum = 0;
  let totalSum = 0;
  let done = 0;
  let failed = 0;
  let canceled = 0;
  let terminal = 0;
  for (const entry of entries) {
    const state = states.get(entry.appId);
    switch (state?.phase) {
      // Every install phase means this DOWNLOAD completed in full — the bytes
      // stay counted so the aggregate never moves backward when an entry
      // continues from "done" into its silent install (Phase 5).
      case "done":
      case "waitingInstall":
      case "installing":
      case "installed":
      case "installFailed":
      case "installCanceled":
        done += 1;
        terminal += 1;
        receivedSum += entry.asset.size;
        totalSum += entry.asset.size;
        break;
      case "failed":
      case "canceled": {
        if (state.phase === "failed") failed += 1;
        else canceled += 1;
        terminal += 1;
        const kept = Math.min(state.received, entry.asset.size);
        receivedSum += kept;
        totalSum += kept;
        break;
      }
      case "downloading":
      case "verifying":
        receivedSum += Math.min(state.received, entry.asset.size);
        totalSum += entry.asset.size;
        break;
      default:
        // Not started yet — its full size is still ahead of us.
        totalSum += entry.asset.size;
        break;
    }
  }
  return {
    fraction: fraction(receivedSum, totalSum),
    done,
    failed,
    canceled,
    finished: entries.length > 0 && terminal === entries.length,
  };
}

export interface InstallProgress {
  /** Aggregate fraction across the install stage (mean of per-app fractions;
   *  a terminal entry counts as settled, the label carries the honesty). */
  fraction: number;
  /** Apps whose installer reported real success. */
  installed: number;
  /** Apps whose install failed or was refused by the trust gate. */
  failed: number;
  /** Apps whose install the user canceled before it started. */
  canceled: number;
}

/**
 * Aggregate progress of the batch's install stage (FC-41): the mean of the
 * per-app staged fractions. Failed/canceled entries count as settled so one
 * refusal never wedges the bar — the summary label states failures explicitly,
 * the bar only ever answers "how far along is this stage". (Settlement itself
 * is owned by the store: the engine's install() resolving, not a flag here.)
 */
export function installProgress(
  ids: readonly string[],
  states: ReadonlyMap<string, AppDownloadState>,
): InstallProgress {
  let sum = 0;
  let installed = 0;
  let failed = 0;
  let canceled = 0;
  for (const id of ids) {
    const state = states.get(id);
    switch (state?.phase) {
      case "installing":
        sum += clamp01(state.fraction);
        break;
      case "installed":
        sum += 1;
        installed += 1;
        break;
      case "installFailed":
        sum += 1;
        failed += 1;
        break;
      case "installCanceled":
        sum += 1;
        canceled += 1;
        break;
      default:
        // waitingInstall (or a state that never entered the stage) — ahead of us.
        break;
    }
  }
  return {
    fraction: ids.length > 0 ? Math.min(sum / ids.length, 1) : 0,
    installed,
    failed,
    canceled,
  };
}
