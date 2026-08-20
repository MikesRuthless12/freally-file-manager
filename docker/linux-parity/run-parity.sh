#!/usr/bin/env bash
# Linux parity run. Mirrors the gates in `.github/workflows/ci.yml`.
#
# Every step reports its own exit code and the script keeps going, so one
# failure does not hide the others — the same reasoning `scripts/ci-local.mjs`
# uses. The final EXITCODE line is what a watcher should assert on; never
# infer success from the absence of error lines.
set -uo pipefail

cd /work

echo "=== rustc / cargo ==="
rustc --version
cargo --version
id
echo

# Root bypasses file permissions, so tests that assert a read-only target
# fails would instead pass silently and report a false failure. The image
# runs as uid 1000 for that reason; refuse to run as root rather than
# produce a result that disagrees with CI.
if [ "$(id -u)" = "0" ]; then
  echo "ERROR: running as root — permission-dependent tests will report"
  echo "false failures. Rebuild the image (it sets USER 1000:1000) or pass"
  echo "--user 1000:1000."
  echo "EXITCODE=1"
  exit 1
fi

FAILED=0
step() {
  local name="$1"; shift
  echo "=== ${name} ==="
  "$@"
  local code=$?
  echo "--- ${name} exit=${code}"
  if [ "${code}" -ne 0 ]; then FAILED=1; fi
  echo
}

# `--locked` so a container run can never rewrite Cargo.lock for the host.
step "fmt"          cargo fmt --all -- --check
step "clippy"       cargo clippy --locked --workspace --all-targets -- -D warnings
step "i18n-lint"    cargo run --locked -p xtask -- i18n-lint
step "overlay-lint" cargo run --locked -p xtask -- overlay-lint

# The point of this harness: the full suite, including the Unix-gated
# tests that never build on Windows.
step "test"         cargo test --locked --workspace --no-fail-fast

echo "EXITCODE=${FAILED}"
