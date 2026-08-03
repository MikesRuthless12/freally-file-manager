#!/usr/bin/env node
// i18n parity + coverage lint (FC-05, extended for the FC-50 panel split).
//  - Two catalog sets: the app's (src/i18n/locales) and the embeddable
//    panel's fcp-* set (src/panel/locales) that host apps load too.
//  - Every locale must define exactly the same message keys as en.ftl,
//    within each set; both sets must ship the same locale list.
//  - The two en catalogs must not overlap (the fcp- prefix guarantees the
//    panel can never collide with a host app's own keys).
//  - Every t("key") used in src must have an en definition in either set.
import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..");
const catalogSets = [
  { name: "app", dir: join(root, "src", "i18n", "locales") },
  { name: "panel", dir: join(root, "src", "panel", "locales") },
];
const srcDir = join(root, "src");

function keysOf(ftl) {
  const keys = new Set();
  for (const line of ftl.split(/\r?\n/)) {
    const m = /^([A-Za-z][\w-]*)\s*=/.exec(line);
    if (m) keys.add(m[1]);
  }
  return keys;
}

function walk(dir) {
  const out = [];
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) out.push(...walk(full));
    else if (/\.(ts|tsx)$/.test(name)) out.push(full);
  }
  return out;
}

let problems = 0;
const enSets = [];
const localeLists = [];

for (const set of catalogSets) {
  const files = readdirSync(set.dir).filter((f) => f.endsWith(".ftl"));
  localeLists.push({ name: set.name, locales: files.map((f) => f.replace(/\.ftl$/, "")).sort() });
  const locales = {};
  for (const file of files) {
    locales[file.replace(/\.ftl$/, "")] = keysOf(readFileSync(join(set.dir, file), "utf8"));
  }
  const en = locales.en;
  if (!en) {
    console.error(`i18n:lint — missing en.ftl in ${set.name} catalogs`);
    process.exit(1);
  }
  enSets.push({ name: set.name, en });
  for (const [loc, keys] of Object.entries(locales)) {
    if (loc === "en") continue;
    const missing = [...en].filter((k) => !keys.has(k));
    const extra = [...keys].filter((k) => !en.has(k));
    if (missing.length || extra.length) {
      problems++;
      console.error(`  ✗ ${set.name}/${loc}: ${missing.length} missing, ${extra.length} extra`);
      if (missing.length) console.error(`      missing: ${missing.join(", ")}`);
      if (extra.length) console.error(`      extra:   ${extra.join(", ")}`);
    }
  }
}

// Every set must ship the same locales, and no two sets may define a key.
const [first, ...restLists] = localeLists;
for (const other of restLists) {
  if (first.locales.join(",") !== other.locales.join(",")) {
    problems++;
    console.error(
      `  ✗ locale lists differ: ${first.name}=[${first.locales}] vs ${other.name}=[${other.locales}]`,
    );
  }
}
for (let i = 0; i < enSets.length; i++) {
  for (let j = i + 1; j < enSets.length; j++) {
    const overlap = [...enSets[i].en].filter((k) => enSets[j].en.has(k));
    if (overlap.length) {
      problems++;
      console.error(
        `  ✗ keys defined in both ${enSets[i].name} and ${enSets[j].name} catalogs: ${overlap.join(", ")}`,
      );
    }
  }
}

// Coverage: keys referenced in source must exist in en (either set).
const used = new Set();
const call = /\bt\(\s*["'`]([A-Za-z][\w-]*)["'`]/g;
for (const file of walk(srcDir)) {
  const text = readFileSync(file, "utf8");
  let m;
  while ((m = call.exec(text))) used.add(m[1]);
}
const unknown = [...used].filter((k) => !enSets.some(({ en }) => en.has(k)));
if (unknown.length) {
  problems++;
  console.error(`  ✗ t() keys used in src with no en definition: ${unknown.join(", ")}`);
}

if (problems) {
  console.error(`i18n:lint — FAILED (${problems} problem${problems > 1 ? "s" : ""})`);
  process.exit(1);
}
const total = enSets.reduce((n, { en }) => n + en.size, 0);
console.log(
  `ok — ${first.locales.length} locales × ${catalogSets.length} catalog sets, ${total} keys, parity + coverage green`,
);
