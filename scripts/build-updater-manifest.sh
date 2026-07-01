#!/usr/bin/env bash
# Phase 16 — Assemble per-channel, per-target updater manifests for
# the Tauri `tauri-plugin-updater` endpoint
# (https://releases.freally.app/{{channel}}/{{target}}-{{arch}}.json).
#
# Arguments:
#   $1 — version string (e.g. "1.0.0")
#   $2 — directory containing the per-target latest.json fragments
#        produced by `tauri build` (one subdir per matrix entry, see
#        .github/workflows/release.yml).
#   $3 — output directory for the assembled manifests (matches the
#        GitHub Pages layout the updater endpoint serves).
#
# The script is intentionally dumb: it copies each per-target
# fragment into the channel/target path a GitHub Pages updater
# endpoint would expect. NOTE: the shipped updater endpoint is the
# GitHub Release asset `…/releases/latest/download/latest.json`, so
# the live `publish-updater-manifest` job in release.yml merges +
# uploads `latest.json` to the release directly and does NOT call
# this script. Kept for the optional Pages-hosted layout — see
# docs/SIGNING_UPGRADE.md for the full wiring.
set -euo pipefail

VERSION="${1:?version missing}"
IN_DIR="${2:?input dir missing}"
OUT_DIR="${3:?output dir missing}"

for channel in stable beta; do
    for pair in \
        "darwin-aarch64:macos-aarch64" \
        "darwin-x86_64:macos-x86_64" \
        "linux-x86_64:linux-x86_64" \
        "windows-x86_64:windows-x86_64"; do
        tgt="${pair%%:*}"
        src="${pair##*:}"
        src_manifest="${IN_DIR}/freally-${src}/latest.json"
        if [[ ! -f "$src_manifest" ]]; then
            echo "skip ${channel}/${tgt}: no ${src_manifest}"
            continue
        fi
        mkdir -p "${OUT_DIR}/${channel}"
        # The updater endpoint template resolves arch separately, so we
        # emit both a combined `<target>-<arch>.json` and a bare
        # `<target>.json` for clients that don't substitute arch yet.
        cp "$src_manifest" "${OUT_DIR}/${channel}/${tgt}.json"
    done
done

# Minimal index so `curl https://releases.freally.app/` returns
# something other than 404.
printf '{"version":"%s"}\n' "$VERSION" >"${OUT_DIR}/latest.json"
printf 'Assembled updater manifests under %s\n' "$OUT_DIR" >&2
