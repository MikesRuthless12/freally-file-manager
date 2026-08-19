// Minimal semver precedence, enough to answer "is the installed version older
// than the latest release?" (FC-21). Not a full parser: it compares the numeric
// release parts, ignores build metadata (`+…`), and orders a prerelease below
// its release ("1.0.0-beta" < "1.0.0") per semver §11.

interface Parsed {
  release: number[];
  prerelease: string[]; // empty = a final release (ranks above any prerelease)
}

function parse(raw: string): Parsed {
  const cleaned = raw.trim().replace(/^v/i, "");
  const withoutBuild = cleaned.split("+", 1)[0];
  const dash = withoutBuild.indexOf("-");
  const mainPart = dash === -1 ? withoutBuild : withoutBuild.slice(0, dash);
  const prePart = dash === -1 ? "" : withoutBuild.slice(dash + 1);
  const release = mainPart
    .split(".")
    .map((n) => Number.parseInt(n, 10))
    .map((n) => (Number.isFinite(n) ? n : 0));
  const prerelease = prePart ? prePart.split(".") : [];
  return { release, prerelease };
}

function compareRelease(a: number[], b: number[]): number {
  const len = Math.max(a.length, b.length);
  for (let i = 0; i < len; i += 1) {
    const diff = (a[i] ?? 0) - (b[i] ?? 0);
    if (diff !== 0) return diff < 0 ? -1 : 1;
  }
  return 0;
}

function comparePrerelease(a: string[], b: string[]): number {
  // A version WITHOUT prerelease outranks one that has it.
  if (a.length === 0 && b.length === 0) return 0;
  if (a.length === 0) return 1;
  if (b.length === 0) return -1;
  const len = Math.max(a.length, b.length);
  for (let i = 0; i < len; i += 1) {
    // Fewer identifiers ranks lower when all preceding ones are equal.
    if (i >= a.length) return -1;
    if (i >= b.length) return 1;
    const ai = a[i];
    const bi = b[i];
    const an = /^\d+$/.test(ai);
    const bn = /^\d+$/.test(bi);
    if (an && bn) {
      const diff = Number.parseInt(ai, 10) - Number.parseInt(bi, 10);
      if (diff !== 0) return diff < 0 ? -1 : 1;
    } else if (an !== bn) {
      // Numeric identifiers always have lower precedence than alphanumeric.
      return an ? -1 : 1;
    } else if (ai !== bi) {
      return ai < bi ? -1 : 1;
    }
  }
  return 0;
}

/**
 * Returns <0 if `a` precedes `b`, 0 if equal, >0 if `a` follows `b`.
 * A leading "v" is tolerated on either side.
 */
export function compareVersions(a: string, b: string): number {
  const pa = parse(a);
  const pb = parse(b);
  const byRelease = compareRelease(pa.release, pb.release);
  if (byRelease !== 0) return byRelease;
  return comparePrerelease(pa.prerelease, pb.prerelease);
}
