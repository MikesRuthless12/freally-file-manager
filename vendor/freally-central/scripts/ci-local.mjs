#!/usr/bin/env node
// Local CI — run the SAME checks as .github/workflows/ci.yml before pushing.
//
// Mirrors the two CI jobs:
//   Rust: cargo fmt --check · clippy -D warnings · test  (+ cargo-deny if present)
//   UI:   typecheck · lint · test · i18n:lint · Playwright e2e
//
// Unlike CI (which stops a job at the first failing step), this runs EVERY check
// and prints one summary at the end, so a single pass surfaces all problems. It
// exits non-zero if anything failed, so it's safe to gate a push on it.
//
// Usage:  node scripts/ci-local.mjs [--no-e2e] [--rust-only] [--ui-only] [--install]
//   --no-e2e     skip the Playwright e2e step (fast inner-loop)
//   --rust-only  run only the Rust checks
//   --ui-only    run only the UI checks
//   --install    (re)install deps first: npm ci + playwright browsers
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const uiDir = join(repoRoot, "ui");

const args = new Set(process.argv.slice(2));
const noE2e = args.has("--no-e2e");
const rustOnly = args.has("--rust-only");
const uiOnly = args.has("--ui-only");
const doInstall = args.has("--install");

// Pass the whole probe as one shell string (not an args array) — with shell:true
// an args array triggers a Node deprecation warning and isn't escaped anyway.
function have(commandLine) {
  return spawnSync(commandLine, { stdio: "ignore", shell: true }).status === 0;
}

const steps = [];
function step(name, cmd, cwd) {
  steps.push({ name, cmd, cwd });
}

const hasRust = existsSync(join(repoRoot, "Cargo.toml")) || existsSync(join(repoRoot, "src-tauri", "Cargo.toml"));
const hasUi = existsSync(join(uiDir, "package.json"));

if (doInstall && hasUi) {
  step("ui: npm ci", "npm ci", uiDir);
  step("ui: playwright install", "npx playwright install --with-deps", uiDir);
}

if (!uiOnly && hasRust) {
  step("rust: fmt", "cargo fmt --all --check", repoRoot);
  step("rust: clippy", "cargo clippy --all-targets --all-features -- -D warnings", repoRoot);
  step("rust: test", "cargo test --all --all-features", repoRoot);
  // CI runs cargo-deny on Linux only; run it locally when it's installed.
  if (have("cargo deny --version")) {
    step("rust: cargo-deny", "cargo deny check", repoRoot);
  } else {
    console.log("• note: cargo-deny not installed — skipping (CI runs it on Linux).");
  }
}

if (!rustOnly && hasUi) {
  step("ui: typecheck", "npm run typecheck", uiDir);
  step("ui: lint", "npm run lint", uiDir);
  step("ui: test", "npm run test", uiDir);
  step("ui: i18n:lint", "npm run i18n:lint", uiDir);
  if (!noE2e) {
    step("ui: e2e", "npm run test:e2e", uiDir);
  } else {
    console.log("• note: --no-e2e — skipping Playwright e2e (CI runs it).");
  }
}

if (steps.length === 0) {
  console.error("ci-local: nothing to run (no Rust/UI detected, or filtered out).");
  process.exit(1);
}

const results = [];
for (const s of steps) {
  const bar = "─".repeat(Math.max(0, 56 - s.name.length));
  console.log(`\n▶ ${s.name} ${bar}`);
  console.log(`  $ ${s.cmd}  (in ${s.cwd === repoRoot ? "." : "ui"})`);
  const started = process.hrtime.bigint();
  const r = spawnSync(s.cmd, { cwd: s.cwd, stdio: "inherit", shell: true });
  const secs = Number((process.hrtime.bigint() - started) / 1_000_000n) / 1000;
  const ok = r.status === 0;
  results.push({ name: s.name, ok, secs });
}

console.log("\n" + "═".repeat(64));
console.log("  Local CI summary");
console.log("═".repeat(64));
let failed = 0;
for (const r of results) {
  const mark = r.ok ? "✓ pass" : "✗ FAIL";
  console.log(`  ${mark}  ${r.name.padEnd(24)} ${r.secs.toFixed(1)}s`);
  if (!r.ok) failed++;
}
console.log("═".repeat(64));

if (failed > 0) {
  console.error(`\n✗ ${failed} check(s) failed — fix before pushing.`);
  process.exit(1);
}
console.log("\n✓ All checks passed — matches CI. Safe to push.");
