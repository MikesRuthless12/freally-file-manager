import type { Page } from "@playwright/test";

/**
 * Seed an accepted EULA (so the gate never blocks a flow test) and clear the
 * catalog/release caches so mocked network routes are always consulted.
 * The one place the storage keys live — every spec imports this.
 */
export async function acceptEula(page: Page): Promise<void> {
  await page.addInitScript(() => {
    try {
      localStorage.setItem("fc.eula.accepted", "2026-07-15");
      for (const key of Object.keys(localStorage)) {
        if (key.startsWith("fc.release.") || key.startsWith("fc.catalog.")) {
          localStorage.removeItem(key);
        }
      }
    } catch {
      /* ignore */
    }
  });
}

/** The live two-decimal percent every progress surface renders (FC-31/41). */
export const PERCENT_RE = /^\d{1,3}[.,]\d{2}\s?%$/;

const MANIFEST_URL = "https://mikesruthless12.github.io/freally-central/freally-central.json";

const WINDOWS_ASSETS = { windows: "\\.(exe|msi)$", macos: "\\.dmg$", linux: "\\.(AppImage|deb|rpm)$" };

// Three downloadable apps so Download & install all has a real queue.
const CATALOG = {
  schemaVersion: 1,
  brand: "Freally",
  apps: [
    { id: "freally-capture", name: "Freally Capture", tagline: "", description: "", features: [], repo: "o/capture", status: "available", assets: WINDOWS_ASSETS },
    { id: "freally-vault", name: "Freally Vault", tagline: "", description: "", features: [], repo: "o/vault", status: "available", assets: WINDOWS_ASSETS },
    { id: "freally-player", name: "Freally Player", tagline: "", description: "", features: [], repo: "o/player", status: "available", assets: WINDOWS_ASSETS },
  ],
};

function releaseFor(repo: string) {
  const app = repo.split("/")[1];
  const name = `${app}_1.0.0_x64-setup.exe`;
  return {
    tag_name: "v1.0.0",
    published_at: "2026-07-10T10:00:00Z",
    html_url: `https://github.com/${repo}/releases/tag/v1.0.0`,
    body: "notes",
    assets: [
      {
        name,
        browser_download_url: `https://github.com/${repo}/releases/download/v1.0.0/${name}`,
        download_count: 10,
        size: 500_000,
        digest: `sha256:${"e".repeat(64)}`,
      },
    ],
  };
}

export interface MockOptions {
  /** Apps whose DOWNLOAD fails partway (scripted "network" error). */
  failDownloadIds?: string[];
  /** Apps whose silent INSTALL fails (scripted installer error). */
  failInstallIds?: string[];
}

/**
 * EULA accepted, caches cleared, catalog + releases mocked, and a scripted
 * download + install engine injected through the app's test-only
 * window.__FC_TEST__ seam (the same one the unit tests use). In production the
 * engine is the Rust backend streaming real bytes and running verified
 * installers; here it is scripted so the flows are deterministic. Progress
 * holds at 100% through the final verify/install beat so the two-decimal
 * "100.00%" is reliably observable.
 */
export async function installMocks(page: Page, options: MockOptions = {}): Promise<void> {
  await acceptEula(page);
  await page.addInitScript(
    ({ failDownloadIds, failInstallIds }: { failDownloadIds: string[]; failInstallIds: string[] }) => {
      const active = new Map<string, { canceled: boolean }>();
      const failDownload = new Set(failDownloadIds);
      const failInstall = new Set(failInstallIds);
      const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
      window.__FC_TEST__ = {
        platform: { os: "windows", arch: "x86_64" },
        downloadEngine: {
          async start(request, onEvent) {
            const entry = { canceled: false };
            active.set(request.id, entry);
            const total = request.expectedSize;
            onEvent({ kind: "started", total, resumedFrom: 0 });
            const steps = 5;
            for (let i = 1; i <= steps; i += 1) {
              await sleep(120);
              if (entry.canceled) {
                onEvent({ kind: "canceled" });
                active.delete(request.id);
                return;
              }
              if (failDownload.has(request.id) && i === 2) {
                onEvent({ kind: "failed", code: "network", detail: "scripted failure" });
                active.delete(request.id);
                return;
              }
              onEvent({ kind: "progress", received: Math.round((total * i) / steps), total });
            }
            onEvent({ kind: "verifying" });
            await sleep(500);
            onEvent({
              kind: "done",
              path: `C:/downloads/${request.fileName}`,
              sha256: "e".repeat(64),
              checksumVerified: true,
            });
            active.delete(request.id);
          },
          cancel(id) {
            const entry = active.get(id);
            if (entry) entry.canceled = true;
          },
          async install(requests, onEvent) {
            // Sequential like the real engine; the staged fraction ends at 1
            // and holds so the aggregate's 100.00% is observable.
            for (const { id } of requests) {
              onEvent({ kind: "started", id });
              let failed = false;
              for (let i = 1; i <= 4; i += 1) {
                await sleep(80);
                if (failInstall.has(id) && i === 2) {
                  onEvent({ kind: "failed", id, code: "installerFailed", detail: "scripted install failure" });
                  failed = true;
                  break;
                }
                onEvent({ kind: "progress", id, fraction: i / 4 });
              }
              if (!failed) {
                await sleep(500);
                onEvent({ kind: "installed", id });
              }
            }
          },
          cancelInstalls() {
            /* nothing pending in the scripted engine */
          },
        },
      };
    },
    { failDownloadIds: options.failDownloadIds ?? [], failInstallIds: options.failInstallIds ?? [] },
  );

  await page.route(MANIFEST_URL, (route) =>
    route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(CATALOG) }),
  );
  await page.route("https://api.github.com/repos/**/releases/latest", async (route) => {
    const repo = /repos\/([^/]+\/[^/]+)\//.exec(route.request().url())?.[1] ?? "o/unknown";
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(releaseFor(repo)),
    });
  });
}
