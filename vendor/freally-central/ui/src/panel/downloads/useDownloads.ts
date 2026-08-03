// The one download + install store (FC-30/31/32 + FC-40/41/42): per-app state
// driven by the engine's event channels, the bounded-concurrency Download All
// queue, and the hands-off install stage that follows it. Hub owns a single
// instance and hands it to the cards, detail view, and toolbar so every
// surface reads identical state.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CatalogApp } from "../catalog/types";
import { liveRelease, type InstallerAsset, type ReleaseState } from "../releases/types";
import { getEngine, getPlatform, type Platform } from "./engine";
import { pickInstaller } from "./pickInstaller";
import { batchProgress, installProgress, isInstallActive, isInstallReady } from "./progress";
import {
  DOWNLOAD_FAIL_CODES,
  INSTALL_FAIL_CODES,
  type AppDownloadState,
  type BatchEntry,
  type DownloadedFile,
  type DownloadEvent,
  type DownloadFailCode,
  type InstallEvent,
  type InstallFailCode,
} from "./types";

// Download All fetches several installers at once, but never unboundedly —
// three keeps the network busy without starving any single transfer (FC-32).
export const MAX_CONCURRENT_DOWNLOADS = 3;

export interface BatchState {
  /** The batch lifecycle, owned here (the store starts and settles it) so no
   *  component has to re-derive "is the batch over" from per-entry states.
   *  "installing" is the hands-off stage after the downloads settle (FC-40). */
  status: "idle" | "running" | "installing" | "settled";
  /** The queued entries (fixed at start; drives the aggregate download bar). */
  entries: BatchEntry[];
  /** The apps whose verified downloads continued into the install stage. */
  installIds: string[];
  /** The outcome counts, frozen at the moment the batch settled — the settled
   *  summary must never be rewritten by later activity on a member app. */
  summary?: BatchSummary;
}

export interface BatchSummary {
  downloadsFailed: number;
  /** Everything that neither finished nor failed when the batch settled —
   *  including queued entries a Cancel all kept from ever starting. */
  downloadsCanceled: number;
  installed: number;
  installsFailed: number;
  installsCanceled: number;
}

/** The batch's outcome counts as they stand right now (frozen at settle).
 *  Totals live on the batch itself (entries.length / installIds.length). */
function summarizeBatch(
  entries: BatchEntry[],
  installIds: string[],
  states: ReadonlyMap<string, AppDownloadState>,
): BatchSummary {
  const downloads = batchProgress(entries, states);
  const installs = installProgress(installIds, states);
  return {
    downloadsFailed: downloads.failed,
    downloadsCanceled: entries.length - downloads.done - downloads.failed,
    installed: installs.installed,
    installsFailed: installs.failed,
    installsCanceled: installs.canceled,
  };
}

export interface DownloadsApi {
  /** True when a real engine is present (Tauri shell, or a test injection). */
  supported: boolean;
  /** Per-app state; absent key = nothing attempted this session. */
  byId: ReadonlyMap<string, AppDownloadState>;
  /** The installer the engine would fetch for this app on this machine. */
  installerFor(app: CatalogApp, release: ReleaseState | undefined): InstallerAsset | null;
  /** Download one app's installer only (also retry — the engine resumes partials). */
  start(appId: string, asset: InstallerAsset): void;
  /** Cancel one app's running download. */
  cancel(appId: string): void;
  /** Silently install one app (FC-40), downloading first only when no
   *  verified file is already on disk. */
  installFlow(appId: string, asset: InstallerAsset): void;
  /** Queue every entry with bounded concurrency, then hand the verified set
   *  straight to the silent installer — Download & install all (FC-32+FC-40). */
  startAll(entries: BatchEntry[]): void;
  /** Cancel the batch: running downloads stop, pending installs cancel (a
   *  running installer finishes and reports its real outcome). */
  cancelAll(): void;
  batch: BatchState;
  /** Apps this session's engine actually installed — flips badges instantly;
   *  a detection re-probe then confirms from the installer's own record. */
  sessionInstalled: ReadonlySet<string>;
  /** Bumped each time an install run settles (Hub re-probes detection on it). */
  installRunsCompleted: number;
}

/** A narrowing function from IPC code strings to a typed set that includes
 *  "unknown" — anything unrecognized maps there. */
function codeNormalizer<T extends string>(codes: readonly T[]): (code: string) => T {
  return (code) =>
    (codes as readonly string[]).includes(code) ? (code as T) : ("unknown" as T);
}

const normalizeFailCode = codeNormalizer<DownloadFailCode>(DOWNLOAD_FAIL_CODES);
const normalizeInstallFailCode = codeNormalizer<InstallFailCode>(INSTALL_FAIL_CODES);

/** The real bytes a state has accounted for (0 when nothing transferred). */
function receivedOf(state: AppDownloadState | undefined): number {
  switch (state?.phase) {
    case "downloading":
    case "verifying":
    case "failed":
    case "canceled":
      return state.received;
    default:
      return 0;
  }
}

/** Map one engine event onto the app's UI state (returning undefined = keep).
 *  Terminal failed/canceled states keep the bytes actually transferred so the
 *  aggregate bar stays monotonic (see batchProgress). */
function reduceEvent(
  previous: AppDownloadState | undefined,
  event: DownloadEvent,
  expectedTotal: number,
): AppDownloadState | undefined {
  switch (event.kind) {
    case "started":
      return { phase: "downloading", received: event.resumedFrom, total: event.total };
    case "progress":
      return { phase: "downloading", received: event.received, total: event.total };
    case "verifying":
      // All bytes are in — hold the bar at its real 100% while the hash runs.
      return { phase: "verifying", received: expectedTotal, total: expectedTotal };
    case "done":
      return { phase: "done", path: event.path, checksumVerified: event.checksumVerified };
    case "failed":
      return { phase: "failed", code: normalizeFailCode(event.code), received: receivedOf(previous) };
    case "canceled":
      return { phase: "canceled", received: receivedOf(previous) };
    default:
      return undefined;
  }
}

/** The finished download every install phase carries forward (the file stays
 *  on disk and the panels keep offering it whatever the install outcome). An
 *  install event with no download behind it (e.g. a "notDownloaded" refusal)
 *  has no file to keep offering. */
function installBase(previous: AppDownloadState | undefined): DownloadedFile {
  return previous && "path" in previous
    ? { path: previous.path, checksumVerified: previous.checksumVerified }
    : { path: "", checksumVerified: false };
}

/** Map one install event onto the app's UI state (returning undefined = keep). */
function reduceInstallEvent(
  previous: AppDownloadState | undefined,
  event: InstallEvent,
): AppDownloadState | undefined {
  const base = installBase(previous);
  switch (event.kind) {
    case "started":
      return { phase: "installing", fraction: 0, ...base };
    case "progress":
      return { phase: "installing", fraction: event.fraction, ...base };
    case "installed":
      return { phase: "installed", ...base };
    case "failed":
      return { phase: "installFailed", code: normalizeInstallFailCode(event.code), ...base };
    case "canceled":
      return { phase: "installCanceled", ...base };
    default:
      return undefined;
  }
}

export function useDownloads(): DownloadsApi {
  const engine = getEngine();
  const supported = engine !== null;

  const [byId, setById] = useState<ReadonlyMap<string, AppDownloadState>>(new Map());
  const [batch, setBatch] = useState<BatchState>({ status: "idle", entries: [], installIds: [] });
  const [platform, setPlatform] = useState<Platform | null>(null);
  const [sessionInstalled, setSessionInstalled] = useState<ReadonlySet<string>>(new Set());
  const [installRunsCompleted, setInstallRunsCompleted] = useState(0);

  // In-flight downloads by app id. Holding the promise (not just a flag) lets
  // a batch worker AWAIT a download that was already started individually
  // instead of skipping past it and finishing the batch early.
  const runningRef = useRef<Map<string, Promise<AppDownloadState | undefined>>>(new Map());
  const batchCanceledRef = useRef(false);
  // The authoritative current state map: async flows (chaining installs after
  // downloads, filtering a queue at its turn) must read state NOW, not the
  // snapshot their closure rendered with.
  const byIdRef = useRef<ReadonlyMap<string, AppDownloadState>>(new Map());
  // Install runs are serialized through this promise chain: the backend holds
  // ONE batch slot, so a second concurrent run would fail every app with
  // "busy" instead of waiting its turn.
  const installQueueRef = useRef<Promise<void>>(Promise.resolve());

  useEffect(() => {
    let alive = true;
    void getPlatform().then((p) => {
      if (alive) setPlatform(p);
    });
    return () => {
      alive = false;
    };
  }, []);

  /** Apply a mutation to the state map — the single write path. The ref is the
   *  source of truth; React state mirrors it for rendering. */
  const applyById = useCallback(
    (mutate: (next: Map<string, AppDownloadState>) => void) => {
      const next = new Map(byIdRef.current);
      mutate(next);
      byIdRef.current = next;
      setById(next);
    },
    [],
  );

  const setAppState = useCallback(
    (appId: string, state: AppDownloadState) => {
      applyById((next) => next.set(appId, state));
    },
    [applyById],
  );

  /** Run one download to its terminal state, which is also resolved so callers
   *  can chain on the real outcome. Never throws (failure isolation). If this
   *  app is already downloading, returns the in-flight promise instead of
   *  restarting it. */
  const runOne = useCallback(
    (appId: string, asset: InstallerAsset): Promise<AppDownloadState | undefined> => {
      if (!engine) return Promise.resolve(undefined);
      const existing = runningRef.current.get(appId);
      if (existing) return existing;
      const task = (async () => {
        // Immediate feedback while the backend spins up the request.
        let last: AppDownloadState = { phase: "downloading", received: 0, total: asset.size };
        setAppState(appId, last);
        try {
          await engine.start(
            {
              id: appId,
              url: asset.url,
              fileName: asset.name,
              expectedSize: asset.size,
              expectedSha256: asset.digest,
            },
            (event) => {
              const next = reduceEvent(last, event, asset.size);
              if (next) {
                last = next;
                setAppState(appId, next);
              }
            },
          );
        } catch {
          // The invoke itself rejected (no terminal event will come).
          last = { phase: "failed", code: "unknown", received: receivedOf(last) };
          setAppState(appId, last);
        } finally {
          runningRef.current.delete(appId);
        }
        return last;
      })();
      runningRef.current.set(appId, task);
      return task;
    },
    [engine, setAppState],
  );

  const start = useCallback(
    (appId: string, asset: InstallerAsset) => {
      void runOne(appId, asset);
    },
    [runOne],
  );

  const cancel = useCallback(
    (appId: string) => {
      engine?.cancel(appId);
    },
    [engine],
  );

  /** One install run to settlement. Never throws. The engine enforces the
   *  trust gate; this only tracks the per-app events it streams back. */
  const doInstallRun = useCallback(
    async (requested: string[]): Promise<void> => {
      if (!engine) return;
      // Filter at this run's actual turn (a preceding queued run may already
      // have installed some of these ids) to the states an install can start
      // from — a verified download, or a failed/canceled earlier attempt.
      const ids = requested.filter((id) => isInstallReady(byIdRef.current.get(id)));
      if (ids.length === 0) return;
      // Queue everyone visibly (a 0% install bar) before the engine starts,
      // carrying the finished download each install phase keeps offering.
      applyById((next) => {
        for (const id of ids) {
          const current = next.get(id);
          if (isInstallReady(current)) {
            next.set(id, { phase: "waitingInstall", ...installBase(current) });
          }
        }
      });
      try {
        await engine.install(
          ids.map((id) => ({ id })),
          (event) => {
            applyById((next) => {
              const reduced = reduceInstallEvent(next.get(event.id), event);
              if (reduced) next.set(event.id, reduced);
            });
            if (event.kind === "installed") {
              setSessionInstalled((prev) => new Set(prev).add(event.id));
            }
          },
        );
      } catch {
        // The invoke itself rejected — no more events will come; anything not
        // terminal is honestly a failure, never silently dropped.
        applyById((next) => {
          for (const id of ids) {
            const state = next.get(id);
            if (state?.phase === "waitingInstall" || state?.phase === "installing") {
              next.set(id, {
                phase: "installFailed",
                code: "unknown",
                path: state.path,
                checksumVerified: state.checksumVerified,
              });
            }
          }
        });
      } finally {
        setInstallRunsCompleted((n) => n + 1);
      }
    },
    [engine, applyById],
  );

  /** Enqueue an install run behind any run already going (solo or batch) —
   *  serialized, so the backend's single batch slot never answers "busy". */
  const runInstalls = useCallback(
    (ids: string[]): Promise<void> => {
      const run = installQueueRef.current.then(() => doInstallRun(ids));
      installQueueRef.current = run.catch(() => {});
      return run;
    },
    [doInstallRun],
  );

  const installFlow = useCallback(
    (appId: string, asset: InstallerAsset) => {
      // A file already verified on disk installs directly — no needless second
      // download. (Store policy, so no caller has to pick the right method.)
      if (isInstallReady(byIdRef.current.get(appId))) {
        void runInstalls([appId]);
        return;
      }
      void runOne(appId, asset).then((terminal) => {
        // Only a verified, completed download continues into the silent
        // install — a failed or canceled one already told its own story.
        if (terminal?.phase === "done") return runInstalls([appId]);
      });
    },
    [runOne, runInstalls],
  );

  const startAll = useCallback(
    (allEntries: BatchEntry[]) => {
      if (!engine) return;
      // An app that is mid-install right now (a solo Install click) belongs to
      // that flow — re-downloading the file its installer is executing would
      // race two writers on its state and can fail the rename on Windows.
      const entries = allEntries.filter(
        (entry) => !isInstallActive(byIdRef.current.get(entry.appId)),
      );
      if (entries.length === 0) return;
      batchCanceledRef.current = false;
      // Clear stale terminal states from earlier downloads so queued entries
      // read as "not started" — otherwise the aggregate bar counts them as
      // already done and moves backward when they actually begin.
      applyById((next) => {
        for (const entry of entries) {
          if (!runningRef.current.has(entry.appId)) next.delete(entry.appId);
        }
      });
      setBatch({ status: "running", entries, installIds: [] });
      const queue = [...entries];
      const succeeded: string[] = [];
      const workers = Array.from(
        { length: Math.min(MAX_CONCURRENT_DOWNLOADS, queue.length) },
        async () => {
          for (;;) {
            const next = queue.shift();
            if (!next || batchCanceledRef.current) return;
            // Each download settles independently — a failure only marks its
            // own card and the worker moves on (FC-32 failure isolation).
            const terminal = await runOne(next.appId, next.asset);
            if (terminal?.phase === "done") succeeded.push(next.appId);
          }
        },
      );
      void Promise.all(workers).then(async () => {
        // Hands-off (FC-40): the verified set continues straight into its
        // silent install — unless the user already canceled the batch.
        const installIds = batchCanceledRef.current ? [] : succeeded;
        if (installIds.length > 0) {
          setBatch((b) => ({ ...b, status: "installing", installIds }));
          await runInstalls(installIds);
        }
        // Freeze the outcome counts: the settled summary reports THIS batch,
        // immune to whatever later runs do to member apps' live state.
        setBatch((b) => ({
          ...b,
          status: "settled",
          summary: summarizeBatch(b.entries, installIds, byIdRef.current),
        }));
      });
    },
    [engine, runOne, runInstalls, applyById],
  );

  const cancelAll = useCallback(() => {
    batchCanceledRef.current = true;
    for (const appId of runningRef.current.keys()) {
      engine?.cancel(appId);
    }
    // Harmless when no install batch runs; during one, pending installs end
    // "canceled" (the engine never kills a running installer).
    engine?.cancelInstalls();
  }, [engine]);

  // A host may unmount the panel (it can live in a closable dialog), removing
  // the only listener on the engine's event channels — this hook owns those
  // channels, so it owns the teardown too: cancel everything still cancelable
  // so nothing runs unobserved. Downloads resume on the next Retry; a running
  // installer (never killed mid-run, by design) finishes in the backend and
  // the next mount's detection re-probe reports its real outcome.
  useEffect(
    () => () => {
      batchCanceledRef.current = true;
      for (const appId of runningRef.current.keys()) {
        engine?.cancel(appId);
      }
      engine?.cancelInstalls();
      // The one cancelable state outside the running registry: an orphaned
      // backend download (alreadyDownloading after a reload).
      for (const [appId, state] of byIdRef.current) {
        if (state.phase === "failed" && state.code === "alreadyDownloading") {
          engine?.cancel(appId);
        }
      }
    },
    [engine],
  );

  const installerFor = useCallback(
    (app: CatalogApp, release: ReleaseState | undefined): InstallerAsset | null => {
      if (!platform || app.status !== "available") return null;
      const live = liveRelease(release);
      return live ? pickInstaller(live.installers[platform.os], platform.os, platform.arch) : null;
    },
    [platform],
  );

  return useMemo(
    () => ({
      supported,
      byId,
      installerFor,
      start,
      cancel,
      installFlow,
      startAll,
      cancelAll,
      batch,
      sessionInstalled,
      installRunsCompleted,
    }),
    [
      supported,
      byId,
      installerFor,
      start,
      cancel,
      installFlow,
      startAll,
      cancelAll,
      batch,
      sessionInstalled,
      installRunsCompleted,
    ],
  );
}
