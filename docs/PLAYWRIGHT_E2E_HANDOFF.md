# Playwright / e2e handoff — Build 3 closeout

**Written:** 2026-08-02, during the Build 3 (v0.22.0) closeout.

Most of this is environment-specific to the machine Build 3 was closed out on,
and is recorded so the next person does not rediscover it. The suite description
and the "how to run" section are durable; the failure diagnosis is a snapshot.

> **Working-copy warning (that machine only).** Three copies of this repo
> existed: `D:\Havoc Software\Freally File Manager` (the real clone — history and
> remote), `C:\Users\miken\Desktop\…` (**no `.git`**, stale, but the directory
> the agent was usually invoked from), and `E:\…` (the previous clone, on exFAT —
> abandoned, see "Why D:" below). Confirm you are in a tree with a `.git` before
> doing anything.

---

## TL;DR

The e2e suite itself is fine and has never been the problem. **117 tests across
24 spec files**, `playwright test --list` enumerates all of them. The blocker is
purely getting a browser binary onto this machine.

At time of writing an install is in flight (see "Current state"). If it
succeeded, run the suite. If it hung again at the same point, the artifact is
genuinely bad and you should stop retrying — the fallback options are listed at
the end.

---

## What the suite is

- Config: `apps/freally-ui/playwright.config.ts`
- Targets the **Vite dev server** at `http://localhost:1420` with a
  `window.__TAURI_INTERNALS__` shim injected by fixtures. **The Tauri binary is
  never booted** — these cover the frontend half of every §4 QA checkbox.
- `webServer` runs `pnpm dev` automatically, `reuseExistingServer` unless CI.
- Chromium only. WebKit/Firefox are commented out deliberately.
- **52 of the 117 are `test.skip`-gated** manual/OS-specific checks (§9
  packaging, §6 cross-platform, parts of §10 release). Those skips are
  **expected** — do not report them as failures.

Verified: **no spec references "Run next" or drag-to-reorder**, so the §3.1
removal (those features were deleted this build) needs no spec changes.

---

## The blocker, precisely

`apps/freally-ui/node_modules/playwright-core/browsers.json` pins **chromium
revision 1217** (Chrome for Testing 147.0.7727.15). That build repeatedly failed
to install:

- Global `%LOCALAPPDATA%\ms-playwright` — left a **3-file stub**, no
  `chrome.exe`, no `INSTALLATION_COMPLETE` marker.
- Private root via `PLAYWRIGHT_BROWSERS_PATH` — three separate attempts, same
  result.

Failure shape every time: the zip download reaches 100% (or stalls at 0%), then
the node process **goes idle at under 1s CPU** with only an empty target
directory created. It never extracts and never exits. Killing it yields
`INSTALL EXIT: -1`, which is the kill, not a real exit code.

### Things ruled out (don't re-test these)

| Hypothesis | Verdict |
|---|---|
| Global browser root contended by another session | **Real, but not the whole story.** The global `__dirlock` *was* genuinely held by another Claude Code session's own hung `playwright install`. Fixed by a private root — the hang persisted anyway. |
| `Start-Process` file redirection deadlocking the progress writer | **Wrong.** Switched to shell-level `cmd /c … > log 2>&1`; identical hang. |
| Just needs the headless shell, not full chromium | **Actively harmful.** Narrowing the request made Playwright *prune* the chromium it had already downloaded: `Removing unused browser at …chromium-1217`. Always request **both** `chromium chromium-headless-shell`. |
| Bump Playwright to a version pinned to 1228 (which installs fine) | **Impossible.** Tested 1.60.0, 1.61.0, 1.62.1 (latest) — **all pin 1217**, same as the current 1.59.1. No released `@playwright/test` pins 1228. The 1228 build in the global cache belongs to the **Playwright MCP server**, which bundles its own copy on a different cadence. This bump was attempted and **reverted**; `package.json` is back to `^1.48` and the lockfile is clean. |

### The most likely actual cause

Another Claude Code session (`Freally Sourcerer`) had its own
`playwright install chromium` **hung for hours** (PIDs 652 / 2780 / 14308, ~1.5s
CPU total), holding the global `__dirlock` and hammering the same CDN. Those were
killed and the lock cleared at ~04:45. **Immediately afterwards the Playwright
MCP successfully launched a browser** (four `chrome-headless-shell` processes),
which it had never managed before.

So the 1217 hangs may have been contention all along rather than a bad artifact.
The in-flight install is the **first attempt in a genuinely clean environment**.

---

## Current state (verify before acting)

```powershell
# Is an install still running?
Get-CimInstance Win32_Process -Filter "Name='node.exe'" |
  Where-Object { $_.CommandLine -match 'playwright.*install' } |
  Select-Object ProcessId, CommandLine

# Did it land?
Test-Path "$env:LOCALAPPDATA\ms-playwright\chromium_headless_shell-1217\chrome-headless-shell-win64\chrome-headless-shell.exe"
Get-ChildItem "$env:LOCALAPPDATA\ms-playwright" | Select-Object Name, LastWriteTime
```

A directory with **3 files and no `INSTALLATION_COMPLETE`** is a failed stub —
delete it before retrying, or Playwright treats it as present.

Known-good in the global cache: `chromium-1228`,
`chromium_headless_shell-1228`, `ffmpeg-1011`, `winldd-1007`.

---

## How to run the suite once a browser exists

Scripts live in the session scratchpad:
`C:\Users\miken\AppData\Local\Temp\claude\C--Users-miken-Desktop-Havoc-Software-Freally-File-Manager\4ea27123-8df9-4b5e-b41e-7d9e67b624d7\scratchpad\`
(`run-e2e.ps1`, `install-browsers.ps1`, `pwinstall.cmd`). These are session-scoped
and will vanish; the commands are reproduced below.

```powershell
# Run DETACHED. The agent harness kills long foreground/background tool tasks.
cd "D:\Havoc Software\Freally File Manager\apps\freally-ui"
pnpm exec playwright test --reporter=list
```

**Two hard rules:**

1. **Do not edit any frontend file while the suite runs.** It drives the Vite dev
   server, and HMR picks the change up mid-run and invalidates the results. This
   already happened once and the run had to be discarded.
2. **Do not re-run the browser install blindly.** Each failed attempt burns ~45
   minutes and a narrowed request prunes browsers you already have.

---

## Fallback options if 1217 still will not install

1. **Playwright MCP** — already connected in-session, uses the global cache and
   works with 1228. It can drive a page and give real "this panel renders"
   evidence, **but it cannot execute the 117 spec files** — it is a browser
   driver, not the test runner. Good enough for the Live-To-Do-List deliverable,
   not for the `pnpm test:e2e` gate. If you go this route you must inject the
   `window.__TAURI_INTERNALS__` shim via `browser_evaluate` first, or every
   panel that calls `invoke` on mount renders an error state and the evidence is
   about a broken page.
2. **`channel: 'chromium'`** in the config makes Playwright use the full browser
   instead of the headless shell. Only helps if a *complete* `chromium-1217`
   exists (it does not right now).
3. **Ship the closeout with e2e unrun and say so explicitly.** This is a
   legitimate outcome — just don't let the Live-To-Do-List imply otherwise.

---

## The actual deliverable this unblocks

`Live-To-Do-List.md` has a **"Playwright confirmed these render"** section. The
roadmap rule is explicit: it is populated **only from a real run**, never guessed
up front. One line per panel/flow, with the run date.

That section is still empty and must stay empty until a real run happens.

---

## Why D:, and other environment landmines

- **`E:` is exFAT** — no hard links, no symlinks. Cargo's incremental cache
  hard-links and silently poisons itself there (`cached cgu … should have an
  object file, but doesn't`, phantom `serde` errors, `E0195` cascades from
  `async_trait`). `pnpm install` fails outright with
  `ERR_PNPM_EISDIR [symlinkAllModules]`. The clone was moved to `D:` (NTFS) on
  2026-08-02 for exactly this reason.
- **Three Claude Code sessions build Rust on this box concurrently** (this one,
  *Freally Sourcerer*, *freally-midi-master-plugin*). They contend for CPU, the
  `~/.cargo` package-cache lock, and the shared `ms-playwright/__dirlock`.
- **Windows Defender real-time scanning is on** and locks freshly written
  `.exe`s. It caused `failed to remove file …build_script_build.exe` during
  `cargo clean`, and one genuine test flake:
  `scenario_05_sharing_violation_retry` (which deliberately takes an *exclusive*
  file lock) lost the race and failed once, then passed 5/5 in isolation.
- **15.7 GB RAM.** Unbounded cargo parallelism put several `link.exe` on large
  Tauri binaries in flight at once and the linker died with **LNK1102 (exit
  1102, out of memory)**. Use `CARGO_BUILD_JOBS=4`.
- **Never kill cargo mid-flight, never `cargo clean` while another cargo runs.**
  Both leave stale fingerprints and invalid metadata that surface as bogus
  `can't find crate` / `found invalid metadata files` / "doesn't implement
  Display" cascades. Recovery cost a full clean plus one uninterrupted rebuild.

---

## Build 3 status at handoff time (context, not Playwright)

Green: `cargo fmt --check` 0 · `cargo clippy -D warnings` 0 ·
`cargo deny` all four sections ok · `svelte-check` 0/0 · `vitest` 2/2 ·
`pnpm build` clean · `cargo test --workspace` 180 binaries / 1796 passed / 0
failed (a later run hit only the Defender flake above).

Still open, and **more important than e2e**:

- **Two HIGH security findings, unfixed.** (1) The FFM-M23 torn-copy marker never
  reaches history — `CopyEvent::Completed` is emitted *before* `SourceChanged`,
  so `source_changed_detail.take()` is always `None` and a torn file is recorded
  as `status: "ok"` with a clean audit entry. (2) The journal launders a tear
  across a resume, so a later move can unlink the source. Both are the exact
  data-loss class this build exists to close. **Do not tag v0.22.0 before these
  are resolved.**
- `xtask i18n-lint` not yet run (needs the cargo build lock).
- CI was red at `0cd0900` for two reasons, both now fixed locally but unpushed:
  the `vendor/freally-central` submodule pointed at a deleted repo (now vendored
  in-tree, 195 files), and `cargo-deny` failed RUSTSEC-2026-0222 (wasmtime
  44.x was never patched; moved to 47.0.3, compiled with zero code changes).
