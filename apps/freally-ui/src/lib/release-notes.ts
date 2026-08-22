/** Release notes as renderable pieces.
 *
 *  The notes come off the network — the `notes` field of the update
 *  manifest, which carries the GitHub release body verbatim. That body is
 *  Markdown, and the Updates pane was rendering it as plain text, so it
 *  showed literal `**Full Changelog**` asterisks and a URL nobody could
 *  click.
 *
 *  Parsing to segments rather than producing an HTML string is deliberate:
 *  the caller renders each segment through normal Svelte interpolation, so
 *  remote text can never reach `{@html}`. There is no sanitiser to get
 *  wrong because no markup is ever produced.
 */

/** One renderable run of the notes. */
export type NoteSegment =
  | { kind: "text"; value: string }
  | { kind: "link"; href: string };

/** Absolute http(s) URLs. Stops at whitespace and at the bracket-ish
 *  characters that usually enclose a URL rather than belong to it. */
const URL_RE = /https?:\/\/[^\s<>"'`)\]}]+/g;

/** Trailing punctuation that reads as sentence punctuation, not URL.
 *  `…/compare/v0.22.0...v1.0.0` must keep its dots, so only strip from
 *  the very end and only one run. */
const TRAILING_PUNCT = /[.,;:!?]+$/;

/** Strip Markdown emphasis markers the notes use for headings like
 *  `**Full Changelog**`. Only the markers go — the text between stays. */
function stripEmphasis(line: string): string {
  return line.replace(/\*\*(.+?)\*\*/g, "$1").replace(/(^|\s)\*(\S.*?\S|\S)\*(?=\s|$)/g, "$1$2");
}

/** Drop paragraphs that repeat one already seen.
 *
 *  Belt-and-braces. The real cause of the four identical
 *  `**Full Changelog**` lines was the release workflow regenerating the
 *  release body once per build target, which is fixed at the source — but
 *  the notes are remote data, and a duplicated paragraph is never
 *  something a reader wants to see twice.
 */
function dropRepeatedParagraphs(raw: string): string {
  const seen = new Set<string>();
  const kept: string[] = [];
  for (const para of raw.split(/\n\s*\n/)) {
    const trimmed = para.trim();
    if (!trimmed) continue;
    const key = trimmed.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    kept.push(trimmed);
  }
  return kept.join("\n\n");
}

/** Split notes into text and link segments, ready to render.
 *
 *  Returns an empty array for empty or whitespace-only input, so the
 *  caller can treat "nothing to show" as a falsy list.
 */
export function parseReleaseNotes(raw: string): NoteSegment[] {
  if (!raw || !raw.trim()) return [];

  const cleaned = stripEmphasis(dropRepeatedParagraphs(raw));
  const out: NoteSegment[] = [];
  let cursor = 0;

  URL_RE.lastIndex = 0;
  for (const match of cleaned.matchAll(URL_RE)) {
    const start = match.index ?? 0;
    let href = match[0];

    // A sentence-ending period belongs to the sentence, not the link.
    const trimmed = href.replace(TRAILING_PUNCT, "");
    const dropped = href.slice(trimmed.length);
    href = trimmed;

    if (start > cursor) {
      out.push({ kind: "text", value: cleaned.slice(cursor, start) });
    }
    out.push({ kind: "link", href });
    if (dropped) out.push({ kind: "text", value: dropped });
    cursor = start + match[0].length;
  }

  if (cursor < cleaned.length) {
    out.push({ kind: "text", value: cleaned.slice(cursor) });
  }
  return out;
}

/** Whether a URL is safe to hand to the OS opener.
 *
 *  The notes are remote, so a `javascript:` or `file:` URL in them must
 *  never reach the opener. `parseReleaseNotes` only ever emits http(s),
 *  but the check lives here too so the click path is safe on its own
 *  terms rather than by trusting its caller.
 */
export function isOpenableUrl(href: string): boolean {
  try {
    const u = new URL(href);
    return u.protocol === "http:" || u.protocol === "https:";
  } catch {
    return false;
  }
}
