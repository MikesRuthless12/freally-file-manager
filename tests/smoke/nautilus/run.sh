#!/usr/bin/env bash
# Phase 7 follow-up — local runner for the Nautilus runtime load test.
#
# Assembles a tiny build context (the GNOME Files extension + this check),
# builds the ubuntu:20.04 image, and runs the check. Mirrors the
# `nautilus-runtime` CI job. Requires Docker. Run from anywhere:
#     bash tests/smoke/nautilus/run.sh
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../../.." && pwd)"
ext="$repo_root/packaging/linux/nautilus/freally_nautilus.py"

ctx="$(mktemp -d)"
trap 'rm -rf "$ctx"' EXIT
cp "$ext" "$ctx/freally_nautilus.py"
cp "$here/nautilus_runtime_check.py" "$ctx/nautilus_runtime_check.py"
cp "$here/Dockerfile" "$ctx/Dockerfile"

docker build -t freally-nautilus-test "$ctx"
docker run --rm freally-nautilus-test
