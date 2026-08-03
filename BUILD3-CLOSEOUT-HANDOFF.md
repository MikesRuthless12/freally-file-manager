# Build 3 (v0.22.0) closeout — handoff

**Written:** 2026-08-02, ~05:15. Successor to `BUILD3-HANDOFF.md`, which described
the bugs; this describes what was done about them and what is left.

**Repo:** `D:\Havoc Software\Freally File Manager` (NTFS).
**Nothing is committed.** `HEAD` is still `0cd0900`. All work below is staged or
unstaged in the working tree. That is deliberate — see "Do not tag yet".

---

## Status update — 2026-08-02, later session

**§A is fixed**, along with nine further data-integrity defects that a code
review and a security review of the fix then surfaced. See
[§A-FIXED](#a-fixed--what-was-actually-done) below; the original diagnosis is
kept underneath it for context.

**The tag is still blocked**, but no longer on §A. It is blocked on a
decision about the pre-existing security surface in
[§F](#f--security-review-backlog-not-fixed) — including plaintext signing keys
in exported profiles and `"csp": null`. Those are yours to triage; none of them
are regressions from this build.

---

## §A-FIXED — what was actually done

Finalization now happens **after** the stability verdict, via a
`PendingFinalize` the caller must resolve one of three ways:

| | journal row | provenance | `Completed` |
|---|---|---|---|
| `commit()` — copy is trustworthy | finished | recorded | emitted |
| `abandon()` — torn, bytes kept (`Warn`) | **invalidated** | omitted | emitted, after `SourceChanged` |
| `discard()` — `Fail` / about to `Recopy` | **invalidated** | omitted | not emitted |

`copy_file` is now the engine's single `Completed` emitter (the atomic-rename
path in `tree.rs` is the one documented exception — it has no read window).
`CopyEvent::Completed` and `CopyEvent::VerifyCompleted` both gained `src`, and
the runner pairs on it instead of guessing "most recently started".

**Divergence from the original plan, deliberate.** The plan below says *"`Warn`:
finalize journal + provenance"*. That contradicts its own symptom §2 and its own
test list: if a torn copy still finalizes the journal, the next run returns
`AlreadyComplete` without opening the source and a later move unlinks it — the
data-loss path is untouched. A torn copy therefore records **nothing**.

**And `abandon()` invalidates the row rather than merely declining to finish
it.** Declining is not enough: the periodic checkpoints hash the *torn read
stream*, and the torn destination holds exactly those bytes, so the next run's
prefix check matches **deterministically**, resume is accepted onto the torn
prefix, and the file is then finalized and certified clean. This needed a new
`JournalSink::invalidate_file`. Getting this wrong once, in this session, is why
it is called out here: "leave the row unfinished" *looks* safe and is not.

### Other defects fixed in the same pass

- **Regression introduced by the first cut of the fix.** `run_async_fallback`
  re-entered the public `copy_file`, so the inner call finalized the journal and
  emitted `Completed` before the outer verdict existed — reinstating §A whole on
  the fallback path (reached by every cross-filesystem copy). The platform
  layer's duplicate async loop is now deleted; `fast_copy` returns
  `Ok(None)` and the engine's own loop runs.
- **CRITICAL, pre-existing — `move_tree` deleted sources it never copied.**
  It hardcodes `TreeOptions::default()` (`collision: Skip`) and its guard checked
  only `source_changed`. Moving onto a partly-populated destination skipped the
  colliding files — zero bytes transferred — then `remove_file`d their sources,
  keeping the destination's older, different content. The runner's sibling
  branch already checked all three counters; `move_tree` now matches.
- **`Fail` could delete a verified-good destination.** The guard bracketed the
  `AlreadyComplete` path, which reads no source bytes but re-hashes the whole
  destination — a minutes-long window on a large file. A `skipped_source_read`
  flag now short-circuits the verdict (defaulting to "guard active", so a path
  that forgets to set it fails safe).
- `VerifyCompleted` carried no `src`, so an exported certificate could present
  another file's hash. Fixed the same way as `Completed`.
- With history disabled, *every* completion took the branch that logged a clean
  `FileCopied` — torn files included.
- The `Recopy` retry snapshotted the torn attempt into version history.
- Map leak on `Failed`; a test fixture that contradicted its own documented
  same-length premise; stale docs in `freally-platform`.

### Tests added

`crates/freally-core/tests/source_stability.rs` is now 17 tests. New: wire
order (`SourceChanged` before its `Completed`); every `Completed` names its own
file across a concurrent 12-file tree; the journal row is invalidated and the
next run is told `Restart`; provenance certifies nothing; `copy_tree` tallies a
torn descendant; cross-device `move_tree` keeps sources when a descendant tore
**and** when a file was skipped. Plus two `freally-journal` unit tests for
`invalidate_file`.

The tear is now reproduced through a `ShapeSink` that rewrites the source from
inside the copy loop. The old hook tore from inside `JournalSink::finish_file`,
which only worked *because* finalization happened before the stamp — it cannot
reproduce a tear any more, precisely because that ordering is what got fixed.

### Known gap, recorded not fixed

The job-level rollup still reports `succeeded` with `files_failed: 0` when a
file tore; only the per-item row says `source-changed`. An exported certificate
therefore reads clean at the header. The natural value, `succeeded_partial`,
exists only as a doc mention in `options.rs` — implementing it means adding a
status the frontend cannot render, which is not a change to make unverified at
tag time.

`forward_events` still has no test at the `ItemRow` level. It needs an
`AppHandle<MockRuntime>`, which would force it and its whole helper chain
generic over the runtime, and a real Wry app cannot build on the headless Linux
CI leg. The defect was an ordering bug in the engine and is covered there.

---

## §A — THE ORIGINAL DIAGNOSIS (kept for context; now fixed)

**Torn-copy finalization happens before the stability verdict.**

Three finalization steps all run inside `copy_file_once`, *before* the wrapper
`copy_file` evaluates the source-stability verdict at `engine.rs:85`:

| Where | What |
|---|---|
| `engine.rs:883-886` | `journal.finish_file(opts.journal_file_idx, final_hash)` |
| `engine.rs:892-898` | provenance sink `record_file(...)` |
| `engine.rs:900-906` | `events.send(CopyEvent::Completed { bytes, duration, rate_bps })` |

`copy_file` awaits `copy_file_once` at `:75`/`:79`, stamps `after` at `:83`, and
only then matches the verdict and emits `CopyEvent::SourceChanged` (`:109` Warn /
`:127` Recopy).

### Three symptoms, one cause

1. **History and audit are wrong.** `runner.rs:715-718` documents "Set by
   `SourceChanged`, consumed by the `Completed` that follows it" — but the wire
   order is the reverse, so `source_changed_detail.take()` at `runner.rs:997` is
   **always `None`**. A torn file gets `ItemRow` status `"ok"` with
   `error_code: None`, and `record_file_copied` (`runner.rs:1029`) writes a clean
   `FileCopied` with a verify hash. In tree mode (default concurrency `Auto`,
   `Semaphore`+`JoinSet` over one shared `tx`) a stale detail is consumed by
   whichever file's `Completed` lands next — false positive on a clean file,
   false negative on the torn one.
   *The activity list is NOT affected* — `runner.rs:958` emits the
   `"source-changed"` `FileActivityDto` straight from the `SourceChanged` arm.
   That is why this looked like it worked.
2. **The journal launders a tear across a resume (irrecoverable).**
   `finish_file` records the torn destination as complete. Next run,
   `decide_resume` returns `AlreadyComplete` and `engine.rs:476-511` returns
   early with `source_changed: None` **without opening the source**. A later
   `move_file` / `move_tree` / trash then unlinks the source on that clean
   report. Survives an app restart; not trash-recoverable.
3. **Provenance certifies torn bytes**, and under `Fail` references a
   destination that `finalize_error` then deletes.

### The fix

Move all three finalizations out of `copy_file_once` into `copy_file`, after the
verdict.

- `copy_file_once` must return what the caller needs: the final BLAKE3
  (`journal_hasher.take()` at `:884`), the provenance `(root, outboard)` pair
  (`:894`), and `copied` / `elapsed` / `rate`.
- **`CopyEvent::Completed` should gain `src`.** It carries none today, which is
  the root of the tree-mode misattribution in symptom 1.
- `Warn`: finalize journal + provenance, then emit `Completed` carrying the
  verdict. `Fail`: finalize neither.
- **`Recopy` needs care.** `engine.rs:137-149` currently detaches the journal
  before the retry *specifically because* `finish_file` already ran (that is the
  §1.2 fix). Once finalization moves out, re-derive that logic — do not just
  delete it.
- There are **five other `CopyEvent::Completed` emitters** (`engine.rs:498`,
  `1520`, `1655`, `1741`, `1797` — resume-AlreadyComplete and the sparse /
  specialized paths). Each needs a decision, not a blind edit.

**Risk:** hottest path in the product. Budget a full verification cycle after
(~40 min here — recipe in §D).

### Tests to add (their absence is why this shipped)

- a torn file's `ItemRow` has status `"source-changed"` — single file **and** in a tree
- resume after a `Warn` tear does not report `AlreadyComplete` and does re-copy
- `move_tree` with a torn descendant (the `tree.rs:497` guard has **zero** coverage)
- `Recopy` where the retry *also* tears still carries the flag out

---

## §B — What was completed this session

### §3.1 — queue UI made honest
`Run next` and drag-to-reorder were removed, not patched: `enqueue_jobs` spawns a
runner per source and `run_job` calls `start()` immediately, so no job is ever
`Pending` and queue order is display-only. Removed the whole vertical slice —
menu item, both `ipc.ts` exports, both Tauri commands + registrations,
`Queue::run_next` / `Queue::reorder` / `QueueEvent::JobReordered`, three tests,
the `action-run-next` key in all 18 locales, the QA drill, and the roadmap +
CHANGELOG + USER_GUIDE claims. **Prioritize/boost was kept — it genuinely works.**

### Two real bugs found and fixed beyond the handoff
- `"source-changed"` was added to the wire type but never to the store's row
  type, so the entire §1.4 UI surface was dead code. Fixed; `ActivityPhase` now
  *derives* from `FileActivityPhase` so the two can never drift again.
- That phase is terminal, but the monotonic guard only knew `done`/`error`, so a
  late `Progress` re-opened the row and hid the warning.

### Security review fixes applied
- **Install rollback regression I introduced:** `scheduler::install` would delete
  a *pre-existing* task when a re-install failed (`schtasks /Create /F` is
  replace-in-place), silently cancelling a working nightly job. Now only
  compensates when the id was not already installed.
- **Trash gate:** `runner.rs` required only `source_changed == 0` before trashing
  a source tree. With `on_error: Skip` or `collision: Skip` over a differing
  destination, files that never transferred had their sources trashed. Now
  requires `errored == 0 && skipped == 0` too.
  *Behaviour note:* a "move, skip identical" now keeps the source rather than
  trashing it. Conservative on purpose — change if you disagree.
- **Linux install rollback gap:** §2.3 gave macOS a plist rollback; Linux has the
  identical write-then-register shape and `linux_impl::is_installed` is a bare
  file-existence probe, so a half-install read as installed. Rollback lifted to
  the `scheduler::install` dispatch so every backend gets it.

### /simplify applied
`engine.rs` encoding repaired (UTF-8 BOM + 102 double-encoded chars in two
passes — `—`, `→`, `…`, `§`, `×`, `💾`; the `×` mojibake's tail is *itself* a
cp1252 em dash, which is why one pass missed it). **That file's diff went from
289 changed lines to 93.** Also: deleted three `_pub` alias wrappers (restoring a
doc comment orphaned onto the wrong function), dropped a redundant `utc_offset`
param across 9 call sites, collapsed the `sync` carry to a whole-struct clone,
fixed two stale doc specs, simplified `path_starts_with`.

### CI unblocked (it has been red since Build 3 was pushed)
- **`vendor/freally-central` submodule pointed at a deleted repo.**
  `actions/checkout` failed outright, so **clippy, the test matrix and the Tauri
  build never ran** (15s and 5s failures). Now vendored in-tree — 195 files
  staged, `.gitmodules` deleted, `submodules: recursive` removed from all three
  workflow checkouts. Only `ui/src/panel` (2 MB of 11 MB) is actually consumed,
  so the rest is prunable later.
- **`cargo-deny` failed RUSTSEC-2026-0222** (wasmtime, via `freally-plugin`).
  The 44.x line was **never patched** — 44.0.3 still fails. Moved to **47.0.3**,
  which compiled with **zero code changes**; sandbox config byte-identical.

---

## §C — Gate status

| Gate | Result |
|---|---|
| `cargo fmt --all -- --check` | ✅ 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ 0 |
| `cargo deny check` | ✅ advisories / bans / licenses / sources all ok |
| `cargo test --workspace` | ✅ 180 binaries, 1796 passed, 0 failed |
| `svelte-check` | ✅ 0 errors / 0 warnings, 180 files |
| `vitest` | ✅ 2/2 |
| `pnpm build` | ✅ clean |
| `xtask i18n-lint` | ⬜ **not run** (needs the cargo build lock) |
| `pnpm test:e2e` | ⬜ **not run** — see `docs/PLAYWRIGHT_E2E_HANDOFF.md` |

### Re-run 2026-08-02 (later session), after the §A fix

| Gate | Result |
|---|---|
| `cargo fmt --all -- --check` | ✅ 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ 0 |
| `cargo deny check` | ✅ advisories / bans / licenses / sources ok |
| `cargo test --workspace` | ✅ 180 binaries, **1805 passed, 0 failed** (was 1796; +9 new) |
| `xtask i18n-lint` | ✅ **now run** — OK, 18 locales × 1281 keys, 931 literal refs |
| `svelte-check` | ✅ 0 errors / 0 warnings, 180 files |
| `vitest` | ✅ 2/2 |
| `pnpm build` | ✅ clean |
| `pnpm test:e2e` | ⚠ **now run** — 47 passed / 18 failed / 52 skipped. All 18 share one root cause; see `Live-To-Do-List.md`. Not a CI gate (CI has no Playwright job). |

The known `scenario_05_sharing_violation_retry` Defender flake did **not**
reproduce in this run.

Clippy and fmt each caught **real pre-existing failures** (an 8-argument
function; a Windows-only `needless_return`; formatting drift) purely because
CI's clippy job had not executed since the submodule broke.

**Known flake:** `scenario_05_sharing_violation_retry`
(`phase_42_integration.rs:662`) takes an *exclusive* file lock to simulate a
sharing violation and loses the race to Defender under load. Failed once, then
passed 5/5 in isolation. Not a defect.

Locale keys: net unchanged at **1281** (`err-schedule-portable` added,
`action-run-next` removed); parity spot-checked across en/de/ja.

---

## §D — Environment (this cost most of the session)

- **`CARGO_BUILD_JOBS=4`.** Unbounded parallelism put several `link.exe` on large
  Tauri binaries in flight at once on a 15.7 GB box and the linker died with
  **LNK1102 (exit 1102, out of memory)**.
- **Run cargo detached.** The agent harness kills long tool tasks, including
  background ones, well before a cold workspace build finishes:
  `Start-Process powershell -ArgumentList "-NoProfile","-File","<script>.ps1" -RedirectStandardOutput <log> -RedirectStandardError <err> -NoNewWindow -PassThru`,
  then poll `tasklist //FI "PID eq <pid>"`. Cargo writes "Compiling" to **stderr**
  and test results to **stdout**, and the redirect is block-buffered — an
  unmoving log does **not** mean a stalled build. Check the process.
- **Never kill cargo mid-flight; never `cargo clean` while another cargo runs.**
  Both leave stale fingerprints / invalid metadata that surface as bogus
  `can't find crate`, `found invalid metadata files for crate`, and
  "doesn't implement `Display`" cascades. Recovery cost a full clean plus one
  uninterrupted rebuild.
- **Other Claude Code sessions build on this machine concurrently** (seen:
  *Freally Sourcerer*, *freally-midi-master-plugin*). They contend for CPU, the
  `~/.cargo` package-cache lock, and `%LOCALAPPDATA%\ms-playwright\__dirlock`.
  Scope any "is cargo running" guard to **this repo**.
- **Defender real-time is on** and locks freshly written `.exe`s — surfaces as
  `failed to remove file …build_script_build.exe` during `cargo clean`. Retry
  clears it; a `target/` exclusion would prevent it.
- **Two stale working copies exist and must not be edited:**
  `C:\Users\miken\Desktop\Havoc Software\Freally File Manager` (**no `.git`** —
  and it is usually the directory the agent is invoked from) and
  `E:\Havoc Software\Freally File Manager` (exFAT; no hard links or symlinks, which
  silently poisons cargo's incremental cache and breaks `pnpm install`).
- Nothing is on the default shell PATH:
  `$env:PATH = "$env:USERPROFILE\.cargo\bin;C:\Program Files\Git\cmd;C:\Program Files\nodejs;$env:APPDATA\npm;$env:PATH"`

---

## §E — Order of work from here

1. **Fix §A.** Then full verification (§D recipe).
2. `cargo run -p xtask --release -- i18n-lint`.
3. e2e — `docs/PLAYWRIGHT_E2E_HANDOFF.md`. Then populate the
   "Playwright confirmed these render" section of `Live-To-Do-List.md`
   **from that real run only** (roadmap rule: never guessed up front).
4. Commit (Conventional Commits), push, **watch CI actually go green** — it has
   not since Build 3.
5. Only then tag `v0.22.0` and run the release train (currently unrun: no tag,
   no release, `docs/index.html` still points at v0.20.0 assets).

## §F — Security review backlog (NOT fixed)

A full security review of the pending change set ran during this session. The
data-integrity findings were fixed (§A-FIXED). Everything below is
**pre-existing, not a regression from this build, and not fixed** — it is a
separate workstream, and several items need frontend changes to verify. Listed
so the tag decision is made with them visible, not discovered later.

Highest first:

1. **Exported profiles serialize secrets in plaintext.** `profiles.rs` does
   `to_string_pretty` over the whole `Settings`; there is no `#[serde(skip)]`
   anywhere on it, and neither carry function touches `server`, `mobile`,
   `recovery` or `crypt`. That puts `server.auth.password`/`token`, webhook URLs
   (which *are* the credential), `recovery.token`, **`mobile.apns_p8_pem` (an
   Apple ECDSA signing private key)** and **`mobile.fcm_service_account_json` (a
   Google service-account RSA key)** into a file the docs describe as shareable
   (`docs/documentation.html`: "export profiles as JSON files for sharing").
   Wants a `Settings::redacted_for_profile()` plus a test asserting sentinel
   secrets never appear in the output.
2. **`"csp": null`** in `tauri.conf.json`. No Content-Security-Policy is
   injected. One line, and it is the multiplier on every item below.
3. **Tauri v2 capabilities do not gate app-defined commands** — only `core:*`
   and plugin permissions. All 229 `#[tauri::command]`s are reachable from any
   webview, including the Drop Stack window, whose capability file's comment
   implies otherwise. Notable unguarded primitives: `server_start` (arbitrary
   root, `0.0.0.0`, `auth.mode: none` — three invokes give read-write WebDAV
   over the whole drive), `add_sync_pair` + `start_sync` (no path validation at
   all; `mirror-right-to-left` mass-deletes), `sanitize_run` (its
   third-confirmation gate compares against a value the backend just handed the
   frontend), `trash_delete`, `system_paste`, `filelist_import` (an
   arbitrary-file-read oracle — non-path lines come back in `missing`).
   `RecoveryDto` already implements exactly the clamp `ServerDto` lacks.
4. **`CopyOptions::dest_jail_root` is dead code.** Defined, checked, never set
   by anything; `safety::is_within_root` has zero callers. Either arm it at the
   IPC boundary or delete it — as it stands it reads like a control that exists.
5. Profile load was narrowed from `carry_backend_owned_from` to
   `carry_install_records_from` in this diff, which means a loaded profile can
   now set `notifications` + `server.webhooks` (outbound exfil, no restart
   needed) and `chunk_store.location_override` (file contents to a UNC path).
   That one **is** from this diff.
6. Scheduler: an error from `is_installed` now falls through to
   `schtasks /Delete /F` (pre-diff it meant "do nothing"); staged task XML is
   written where a redirected `LOCALAPPDATA` could be raced; no ownership check
   on `\Freally\<slug>` task names.
7. Settings and profile files are written with bare `fs::write` — 0644 on Unix,
   holding the `.p8` key.

**Not covered by the review at all:** `vendor/freally-central` (~24k of the
27k added lines — the whole update panel and its GitHub release fetching), the
Svelte frontend diff (which matters more given item 2), `freally-server`,
`freally-cloud`, `freally-crypt`, `freally-helper`, `freally-shellext`, SQL
construction in `freally-history`, and the `Cargo.lock` supply-chain delta.

---

### Deferred, recorded, not done
Duplicate per-file `stat` in `engine.rs` (`SourceStamp::of` re-stats what
`copy_file_once` stats — real, but hot path); `staging_dir` inventing a third
app-directory convention; `nonce()` vs the workspace's `getrandom` pattern;
per-volume case sensitivity via `VolumeProbe`; making the activity phase and
history status real enums instead of stringly-typed.

### Worth knowing, pre-existing, out of scope
`Settings::server` (auth token, password, webhook URLs, `pushover_token`) is
carried by **neither** carry function, so **saved profiles contain those in
plaintext** — and profiles are described as shareable. The cross-device
torn-move test only actually runs on Linux; it silently passes on the Windows and
macOS legs.
