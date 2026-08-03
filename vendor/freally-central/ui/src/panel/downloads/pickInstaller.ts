// Choose "the right per-OS installer" (FC-30) from a release's matched assets.
//
// Preference order per OS mirrors what the later silent-install phase can run
// unattended: Windows NSIS setup.exe over MSI; macOS .dmg; Linux AppImage
// (universal) over .deb over .rpm. When asset names carry an architecture token
// (x64 / aarch64 / …), only assets compatible with this machine are considered;
// name-agnostic assets are treated as universal.

import type { InstallerAsset, OsKey } from "../releases/types";

// Filename tokens per architecture, as Tauri's bundler emits them. Names are
// canonicalized ("x86_64"/"x86-64" → "x64") before token checks so the 32-bit
// "x86" token can never fire inside a 64-bit name.
const ARCH_TOKENS: Record<string, string[]> = {
  x86_64: ["x64", "amd64"],
  aarch64: ["arm64", "aarch64"],
  x86: ["x86", "i686", "ia32"],
};

const ALL_ARCH_TOKENS = Object.values(ARCH_TOKENS).flat();

// The token set is fixed, so each token's boundary regex is compiled once —
// installer selection runs on every release refresh across the whole grid.
const TOKEN_PATTERNS = new Map<string, RegExp>(
  ALL_ARCH_TOKENS.map((token) => [
    token,
    new RegExp(`(?:^|[^a-z0-9])${token}(?:$|[^a-z0-9])`, "i"),
  ]),
);

function canonicalName(name: string): string {
  return name.replace(/x86[_-]64/gi, "x64");
}

// Highest-priority first; an asset must match one to be considered at all.
const EXTENSION_PREFERENCE: Record<OsKey, RegExp[]> = {
  windows: [/-setup\.exe$/i, /\.exe$/i, /\.msi$/i],
  macos: [/\.dmg$/i],
  linux: [/\.appimage$/i, /\.deb$/i, /\.rpm$/i],
};

/** True when the name mentions an arch token, matched on token boundaries
 *  (so the "x86" token never fires inside "x86_64"). */
function mentionsToken(name: string, token: string): boolean {
  return TOKEN_PATTERNS.get(token)?.test(name) ?? false;
}

/** Arch-compatible = names our arch, or names no arch at all (universal). */
function archCompatible(name: string, arch: string): boolean {
  const canon = canonicalName(name);
  const ours = ARCH_TOKENS[arch] ?? [];
  if (ours.some((t) => mentionsToken(canon, t))) return true;
  return !ALL_ARCH_TOKENS.some((t) => mentionsToken(canon, t));
}

/**
 * The installer to download on this machine, or null when the release ships
 * nothing suitable. `arch` is Rust's std::env::consts::ARCH ("x86_64", …).
 */
export function pickInstaller(
  assets: InstallerAsset[] | undefined,
  os: OsKey,
  arch: string,
): InstallerAsset | null {
  if (!assets || assets.length === 0) return null;
  const compatible = assets.filter((a) => archCompatible(a.name, arch));
  for (const pattern of EXTENSION_PREFERENCE[os]) {
    const hits = compatible.filter((a) => pattern.test(a.name));
    if (hits.length === 0) continue;
    // Prefer an arch-exact name over a universal one when both match.
    const ours = ARCH_TOKENS[arch] ?? [];
    return hits.find((a) => ours.some((t) => mentionsToken(canonicalName(a.name), t))) ?? hits[0];
  }
  return null;
}
