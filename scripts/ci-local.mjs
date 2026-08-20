#!/usr/bin/env node
// Local CI — run the SAME checks as .github/workflows/ci.yml before pushing.
//
// Mirrors the CI jobs that can run on a developer workstation:
//   Rust (repo root):
//     fmt         · cargo fmt --all -- --check                      (job: rustfmt)
//     i18n-lint   · cargo run -p xtask -- i18n-lint                 (job: i18n-lint)
//     clippy      · cargo clippy --workspace --all-targets -- -D warnings   (job: clippy-test)
//     test        · cargo test --workspace  (incl. doctests)        (job: clippy-test)
//     spawn smoke · cargo test -p freally-helper --features spawn-tests
//                     --test phase_17d_spawn                        (job: clippy-test)
//     cargo-deny  · cargo deny check   (job: cargo-deny;  run if installed)
//     cargo-audit · cargo audit --ignore ...          (job: cargo-audit; run if installed)
//     cargo-vet   · cargo vet --locked                (job: cargo-vet;   run if installed,
//                                                       non-blocking — CI: continue-on-error)
//   UI (apps/freally-ui, pnpm):
//     tauri build · pnpm tauri build --debug --no-bundle            (job: tauri-build)
//
// NOT mirrored: the `nautilus-runtime` job runs only inside an ubuntu:20.04
// container against the GNOME Files typelib, so it can't run on a normal dev box.
//
// Unlike CI (which stops a job at the first failing step, and runs jobs on a
// 3-OS matrix), this runs EVERY check on the host OS and prints one summary at
// the end, so a single pass surfaces all problems. It exits non-zero if any
// required check failed, so it's safe to gate a push on it.
//
// Two scheduling notes, both worth preserving if you edit this:
//
//   * cargo-deny / cargo-audit / cargo-vet read Cargo.lock and an advisory
//     database. They never touch `target/` and never take cargo's build lock,
//     so they run CONCURRENTLY with the compile chain instead of adding their
//     wall-clock to it. Their output is captured and replayed under its own
//     heading so it cannot interleave with the compile logs.
//   * Everything that compiles stays STRICTLY SERIAL. Two cargo builds against
//     one target dir only block on the lock, and interrupting them is how the
//     fingerprint cache gets poisoned.
//
// Usage:  node scripts/ci-local.mjs [--rust-only] [--ui-only] [--install]
//   --rust-only  run only the Rust checks (skip the slow Tauri build)
//   --ui-only    run only the UI / Tauri build check
//   --install    (re)install UI deps first: pnpm install --frozen-lockfile
import { spawn, spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const uiDir = join(repoRoot, "apps", "freally-ui");

const args = new Set(process.argv.slice(2));
const rustOnly = args.has("--rust-only");
const uiOnly = args.has("--ui-only");
const doInstall = args.has("--install");

// Incremental state only pays off across repeated builds of the same tree; a
// gate run recompiles what changed and then hands the artifacts to nobody.
// (Debug-info trimming lives in the workspace Cargo.toml so it applies to a
// plain `cargo test` too, not only to this script.)
const childEnv = { ...process.env, CARGO_INCREMENTAL: "0" };

// Pass the whole probe as one shell string (not an args array) — with shell:true
// an args array triggers a Node deprecation warning and isn't escaped anyway.
function have(commandLine) {
  return spawnSync(commandLine, { stdio: "ignore", shell: true }).status === 0;
}

const serial = [];
const concurrent = [];
function step(name, cmd, cwd, { optional = false } = {}) {
  serial.push({ name, cmd, cwd, optional });
}
// A check that takes no build lock, so it overlaps the compile chain.
function bgStep(name, cmd, cwd, { optional = false } = {}) {
  concurrent.push({ name, cmd, cwd, optional });
}

const hasRust = existsSync(join(repoRoot, "Cargo.toml"));
const hasUi = existsSync(join(uiDir, "package.json"));

// cargo-audit ignore list — kept in lockstep with the `cargo-audit` job in
// .github/workflows/ci.yml (which itself mirrors deny.toml's [advisories]
// ignore block). Update all three atomically when adding/removing an exception.
const auditIgnores = [
  "RUSTSEC-2023-0071", "RUSTSEC-2021-0119", "RUSTSEC-2025-0026", "RUSTSEC-2021-0154",
  "RUSTSEC-2020-0168", "RUSTSEC-2025-0052", "RUSTSEC-2024-0411", "RUSTSEC-2024-0412",
  "RUSTSEC-2024-0413", "RUSTSEC-2024-0414", "RUSTSEC-2024-0415", "RUSTSEC-2024-0416",
  "RUSTSEC-2024-0417", "RUSTSEC-2024-0418", "RUSTSEC-2024-0419", "RUSTSEC-2024-0420",
  "RUSTSEC-2024-0429", "RUSTSEC-2024-0370", "RUSTSEC-2026-0173", "RUSTSEC-2025-0057",
  "RUSTSEC-2025-0075", "RUSTSEC-2025-0080", "RUSTSEC-2025-0081", "RUSTSEC-2025-0098",
  "RUSTSEC-2025-0100", "RUSTSEC-2024-0436", "RUSTSEC-2026-0097",
  "RUSTSEC-2026-0194", "RUSTSEC-2026-0195",
];

if (!uiOnly && hasRust) {
  // job: rustfmt
  step("rust: fmt", "cargo fmt --all -- --check", repoRoot);
  // job: i18n-lint — debug, not release. This is a key-parity check over 18
  // locale files, not a compute-bound one, and `--release` meant building the
  // whole xtask dependency tree in a profile no other step here reuses.
  step("rust: i18n-lint", "cargo run -p xtask -- i18n-lint", repoRoot);
  // job: overlay-lint — same shape as i18n-lint, and the consumer that
  // `escapeToClose`'s `data-escape-closes` marker never had. Two drawers
  // shipped without Escape-to-close because nothing checked.
  step("rust: overlay-lint", "cargo run -p xtask -- overlay-lint", repoRoot);
  // job: clippy-test
  step("rust: clippy", "cargo clippy --workspace --all-targets -- -D warnings", repoRoot);
  // Deliberately NOT cargo-nextest, even when it is installed. It cannot
  // build this workspace: every freally-ui test target fails with
  // `can't find crate for freally_ui_lib` and `crate <dep> required to be
  // available in rlib format`, despite the crate already declaring
  // crate-type = ["staticlib", "cdylib", "rlib"]. Plain
  // `cargo test --workspace --no-run` builds the same targets clean, so this
  // is a nextest/Tauri interaction rather than something to configure away.
  // `cargo test` also runs the doctests, which nextest skips outright.
  step("rust: test", "cargo test --workspace", repoRoot);
  step(
    "rust: spawn smoke",
    "cargo test -p freally-helper --features spawn-tests --test phase_17d_spawn",
    repoRoot,
  );
  // job: cargo-deny — CI uses EmbarkStudios/cargo-deny-action; run the CLI locally when present.
  if (have("cargo deny --version")) {
    bgStep("rust: cargo-deny", "cargo deny check", repoRoot);
  } else {
    console.log("• note: cargo-deny not installed — skipping (CI runs it via cargo-deny-action).");
  }
  // job: cargo-audit — CI installs a prebuilt binary; run locally only if already available.
  if (have("cargo audit --version")) {
    const ignores = auditIgnores.map((id) => "--ignore " + id).join(" ");
    bgStep("rust: cargo-audit", "cargo audit " + ignores, repoRoot);
  } else {
    console.log("• note: cargo-audit not installed — skipping (CI installs a prebuilt binary).");
  }
  // job: cargo-vet — non-blocking in CI (continue-on-error); mark optional here too.
  if (have("cargo vet --version")) {
    bgStep("rust: cargo-vet", "cargo vet --locked", repoRoot, { optional: true });
  } else {
    console.log("• note: cargo-vet not installed — skipping (CI installs it; non-blocking).");
  }
}

if (!rustOnly && hasUi) {
  // job: tauri-build — CI installs deps then does a --no-bundle debug compile.
  // Stays in the serial chain: it drives cargo against the same target dir.
  if (doInstall) {
    step("ui: pnpm install", "pnpm install --frozen-lockfile", uiDir);
  }
  step("ui: tauri build", "pnpm tauri build --debug --no-bundle", uiDir);
}

if (serial.length === 0 && concurrent.length === 0) {
  console.error("ci-local: nothing to run (no Rust/UI detected, or filtered out).");
  process.exit(1);
}

const label = (cwd) => (cwd === repoRoot ? "." : "apps/freally-ui");
const secondsSince = (t0) => Number((process.hrtime.bigint() - t0) / 1000000n) / 1000;

// Captured, so concurrent output cannot interleave with the compile logs.
function runCaptured(s) {
  return new Promise((resolve) => {
    const started = process.hrtime.bigint();
    const child = spawn(s.cmd, { cwd: s.cwd, shell: true, env: childEnv });
    // Without this the promise never settles if the shell itself cannot be
    // spawned, and Node turns the unhandled "error" event into a crash. The
    // spawnSync this replaced degraded to a failed step instead.
    child.on("error", (e) =>
      resolve({ name: s.name, ok: false, secs: secondsSince(started), optional: s.optional, out: String(e) }),
    );
    let out = "";
    child.stdout.on("data", (d) => (out += d));
    child.stderr.on("data", (d) => (out += d));
    child.on("close", (code) =>
      resolve({
        name: s.name,
        ok: code === 0,
        secs: secondsSince(started),
        optional: s.optional,
        out,
      }),
    );
  });
}

// Live output — this is the chain the developer is actually waiting on.
function runInherit(s) {
  return new Promise((resolve) => {
    const bar = "─".repeat(Math.max(0, 56 - s.name.length));
    console.log("\n▶ " + s.name + " " + bar);
    console.log("  $ " + s.cmd + "  (in " + label(s.cwd) + ")");
    const started = process.hrtime.bigint();
    const child = spawn(s.cmd, { cwd: s.cwd, stdio: "inherit", shell: true, env: childEnv });
    child.on("error", (e) => {
      console.error("  spawn failed: " + e.message);
      resolve({ name: s.name, ok: false, secs: secondsSince(started), optional: s.optional });
    });
    child.on("close", (code) =>
      resolve({ name: s.name, ok: code === 0, secs: secondsSince(started), optional: s.optional }),
    );
  });
}

// Start the lock-free audits first so they overlap the compile chain, then walk
// the chain one step at a time.
const bgPromise = Promise.all(concurrent.map(runCaptured));
if (concurrent.length > 0) {
  console.log(
    "• running " + concurrent.map((s) => s.name).join(", ") + " alongside the compile chain.",
  );
}

const results = [];
for (const s of serial) results.push(await runInherit(s));

const bgResults = await bgPromise;
for (const r of bgResults) {
  const bar = "─".repeat(Math.max(0, 40 - r.name.length));
  console.log("\n▶ " + r.name + " (ran concurrently) " + bar);
  process.stdout.write(r.out);
  results.push({ name: r.name, ok: r.ok, secs: r.secs, optional: r.optional });
}

console.log("\n" + "═".repeat(64));
console.log("  Local CI summary");
console.log("═".repeat(64));
let failed = 0;
for (const r of results) {
  const mark = r.ok ? "✓ pass" : r.optional ? "! warn" : "✗ FAIL";
  console.log("  " + mark + "  " + r.name.padEnd(24) + " " + r.secs.toFixed(1) + "s");
  if (!r.ok && !r.optional) failed++;
}
console.log("═".repeat(64));

if (failed > 0) {
  console.error("\n✗ " + failed + " required check(s) failed — fix before pushing.");
  process.exit(1);
}
console.log("\n✓ All required checks passed — matches CI. Safe to push.");
