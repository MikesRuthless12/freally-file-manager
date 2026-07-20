#!/usr/bin/env node
// Local CI — run the SAME checks as .github/workflows/ci.yml before pushing.
//
// Mirrors the CI jobs that can run on a developer workstation:
//   Rust (repo root):
//     fmt         · cargo fmt --all -- --check                      (job: rustfmt)
//     i18n-lint   · cargo run -p xtask --release -- i18n-lint        (job: i18n-lint)
//     clippy      · cargo clippy --workspace --all-targets -- -D warnings   (job: clippy-test)
//     test        · cargo test --workspace                          (job: clippy-test)
//     spawn smoke · cargo test -p freally-helper --features spawn-tests
//                     --test phase_17d_spawn                        (job: clippy-test)
//     cargo-deny  · cargo deny check --all-features   (job: cargo-deny;  run if installed)
//     cargo-audit · cargo audit --ignore …            (job: cargo-audit; run if installed)
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
// Usage:  node scripts/ci-local.mjs [--rust-only] [--ui-only] [--install]
//   --rust-only  run only the Rust checks (skip the slow Tauri build)
//   --ui-only    run only the UI / Tauri build check
//   --install    (re)install UI deps first: pnpm install --frozen-lockfile
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const uiDir = join(repoRoot, "apps", "freally-ui");

const args = new Set(process.argv.slice(2));
const rustOnly = args.has("--rust-only");
const uiOnly = args.has("--ui-only");
const doInstall = args.has("--install");

// Pass the whole probe as one shell string (not an args array) — with shell:true
// an args array triggers a Node deprecation warning and isn't escaped anyway.
function have(commandLine) {
  return spawnSync(commandLine, { stdio: "ignore", shell: true }).status === 0;
}

const steps = [];
function step(name, cmd, cwd, { optional = false } = {}) {
  steps.push({ name, cmd, cwd, optional });
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
];

if (!uiOnly && hasRust) {
  // job: rustfmt
  step("rust: fmt", "cargo fmt --all -- --check", repoRoot);
  // job: i18n-lint
  step("rust: i18n-lint", "cargo run -p xtask --release -- i18n-lint", repoRoot);
  // job: clippy-test
  step("rust: clippy", "cargo clippy --workspace --all-targets -- -D warnings", repoRoot);
  step("rust: test", "cargo test --workspace", repoRoot);
  step(
    "rust: spawn smoke",
    "cargo test -p freally-helper --features spawn-tests --test phase_17d_spawn",
    repoRoot,
  );
  // job: cargo-deny — CI uses EmbarkStudios/cargo-deny-action; run the CLI locally when present.
  if (have("cargo deny --version")) {
    step("rust: cargo-deny", "cargo deny check --all-features", repoRoot);
  } else {
    console.log("• note: cargo-deny not installed — skipping (CI runs it via cargo-deny-action).");
  }
  // job: cargo-audit — CI `cargo install`s it; run locally only if already available.
  if (have("cargo audit --version")) {
    const ignores = auditIgnores.map((id) => `--ignore ${id}`).join(" ");
    step("rust: cargo-audit", `cargo audit ${ignores}`, repoRoot);
  } else {
    console.log("• note: cargo-audit not installed — skipping (CI installs it fresh).");
  }
  // job: cargo-vet — non-blocking in CI (continue-on-error); mark optional here too.
  if (have("cargo vet --version")) {
    step("rust: cargo-vet", "cargo vet --locked", repoRoot, { optional: true });
  } else {
    console.log("• note: cargo-vet not installed — skipping (CI installs it; non-blocking).");
  }
}

if (!rustOnly && hasUi) {
  // job: tauri-build — CI installs deps then does a --no-bundle debug compile.
  if (doInstall) {
    step("ui: pnpm install", "pnpm install --frozen-lockfile", uiDir);
  }
  step("ui: tauri build", "pnpm tauri build --debug --no-bundle", uiDir);
}

if (steps.length === 0) {
  console.error("ci-local: nothing to run (no Rust/UI detected, or filtered out).");
  process.exit(1);
}

const results = [];
for (const s of steps) {
  const label = s.cwd === repoRoot ? "." : "apps/freally-ui";
  const bar = "─".repeat(Math.max(0, 56 - s.name.length));
  console.log(`\n▶ ${s.name} ${bar}`);
  console.log(`  $ ${s.cmd}  (in ${label})`);
  const started = process.hrtime.bigint();
  const r = spawnSync(s.cmd, { cwd: s.cwd, stdio: "inherit", shell: true });
  const secs = Number((process.hrtime.bigint() - started) / 1_000_000n) / 1000;
  const ok = r.status === 0;
  results.push({ name: s.name, ok, secs, optional: s.optional });
}

console.log("\n" + "═".repeat(64));
console.log("  Local CI summary");
console.log("═".repeat(64));
let failed = 0;
for (const r of results) {
  const mark = r.ok ? "✓ pass" : r.optional ? "! warn" : "✗ FAIL";
  console.log(`  ${mark}  ${r.name.padEnd(24)} ${r.secs.toFixed(1)}s`);
  if (!r.ok && !r.optional) failed++;
}
console.log("═".repeat(64));

if (failed > 0) {
  console.error(`\n✗ ${failed} required check(s) failed — fix before pushing.`);
  process.exit(1);
}
console.log("\n✓ All required checks passed — matches CI. Safe to push.");
