<!--
  FFM-M09 — hash inspector + clipboard compare.

  Pick any mix of files and folders, hash them with any of the eight
  algorithms, then either paste a digest to compare every row against
  or pick exactly two files and compare them to each other. The
  Identical / Different treatment is the collision dialog's, reused
  verbatim so the two verdict surfaces read the same.

  Opened from the footer, or seeded by Explorer's "Hash with Freally
  File Manager" verb / `freally --hash <paths…>` — a seeded open hashes
  immediately.
-->
<script lang="ts">
  import { untrack } from "svelte";

  import Icon from "../icons/Icon.svelte";
  import { t } from "../i18n";
  import { formatBytes } from "../format";
  import { closeHashInspector, pushToast } from "../stores";
  import {
    HASH_ALGORITHMS,
    hashInspect,
    hashParseDigest,
    pickFiles,
    pickFolders,
    type HashRow,
  } from "../ipc";

  interface Props {
    /** Selection to seed the panel with; empty for a footer open. */
    seedPaths: string[];
  }

  let { seedPaths }: Props = $props();

  let algo = $state("sha256");
  let paths = $state<string[]>([]);
  let rows = $state<HashRow[]>([]);
  let truncated = $state(false);
  let busy = $state(false);

  // The pasted comparison digest: raw text as typed, plus the parsed
  // lowercase hex (null when the text holds nothing digest-shaped).
  let pasted = $state("");
  let expected = $state<string | null>(null);

  // Two-file compare: only meaningful when nothing was pasted, both
  // files hashed cleanly, and the selection is exactly a pair.
  let pairIdentical = $derived.by(() => {
    if (expected !== null || rows.length !== 2) return null;
    if (!rows[0].hex || !rows[1].hex) return null;
    return rows[0].hex === rows[1].hex;
  });

  function verdictFor(row: HashRow): boolean | null {
    if (expected === null || !row.hex) return null;
    return row.hex === expected;
  }

  async function run(list: string[]): Promise<void> {
    if (list.length === 0 || busy) return;
    busy = true;
    try {
      const report = await hashInspect(list, algo);
      rows = report.rows;
      truncated = report.truncated;
    } catch (err) {
      pushToast("error", err instanceof Error ? err.message : String(err));
    } finally {
      busy = false;
    }
  }

  async function add(picker: () => Promise<string[]>): Promise<void> {
    try {
      const picked = await picker();
      if (picked.length === 0) return;
      // De-duplicate so picking the same file twice doesn't hash it twice.
      paths = [...new Set([...paths, ...picked])];
      // The digests on screen belong to the previous selection.
      rows = [];
      truncated = false;
    } catch (err) {
      pushToast("error", err instanceof Error ? err.message : String(err));
    }
  }

  function clear(): void {
    paths = [];
    rows = [];
    truncated = false;
  }

  async function onPastedInput(): Promise<void> {
    if (pasted.trim() === "") {
      expected = null;
      return;
    }
    // Typing races: each keystroke starts a round-trip, and they can
    // land out of order. Only the answer for what is *currently* in the
    // field may set the verdict.
    const asked = pasted;
    try {
      const parsed = await hashParseDigest(asked);
      if (asked === pasted) expected = parsed;
    } catch {
      // A parse failure is indistinguishable from "not a digest" to
      // the user; both show the hint below the field.
      if (asked === pasted) expected = null;
    }
  }

  function onKeydown(e: KeyboardEvent): void {
    if (e.key === "Escape") closeHashInspector();
  }

  // Seed (and re-seed) from the store. A second Explorer "Hash with
  // Freally File Manager" while the panel is already open replaces the
  // selection and re-hashes. Every write below is untracked so the
  // reset never re-triggers the effect — `seedPaths` is the only
  // dependency, and the store always hands over a fresh array.
  $effect(() => {
    const seeds = seedPaths;
    untrack(() => {
      paths = [...seeds];
      rows = [];
      truncated = false;
      if (seeds.length > 0) void run(seeds);
    });
  });
</script>

<div
  class="backdrop"
  role="dialog"
  aria-modal="true"
  aria-labelledby="hash-inspector-title"
  tabindex="-1"
  onkeydown={onKeydown}
>
  <div class="panel">
    <h2 id="hash-inspector-title">{t("hash-inspector-title")}</h2>

    <div class="controls">
      <label class="algo">
        <span>{t("hash-inspector-algo-label")}</span>
        <select bind:value={algo} disabled={busy}>
          {#each HASH_ALGORITHMS as name (name)}
            <option value={name}>{name}</option>
          {/each}
        </select>
      </label>
      <button
        type="button"
        class="btn"
        disabled={busy}
        onclick={() => add(pickFiles)}
      >
        <Icon name="file" size={14} />
        {t("hash-inspector-add-files")}
      </button>
      <button
        type="button"
        class="btn"
        disabled={busy}
        onclick={() => add(pickFolders)}
      >
        <Icon name="folder" size={14} />
        {t("hash-inspector-add-folders")}
      </button>
      <button
        type="button"
        class="btn primary"
        disabled={busy || paths.length === 0}
        onclick={() => run(paths)}
      >
        {busy ? t("hash-inspector-hashing") : t("hash-inspector-run")}
      </button>
      <button
        type="button"
        class="btn"
        disabled={busy || paths.length === 0}
        onclick={clear}
      >
        {t("hash-inspector-clear")}
      </button>
    </div>

    <label class="compare">
      <span>{t("hash-inspector-compare-label")}</span>
      <input
        type="text"
        spellcheck="false"
        placeholder={t("hash-inspector-compare-placeholder")}
        bind:value={pasted}
        oninput={onPastedInput}
      />
    </label>
    {#if pasted.trim() !== ""}
      {#if expected === null}
        <p class="hint bad">{t("hash-inspector-compare-invalid")}</p>
      {:else}
        <p class="hint"><code>{expected}</code></p>
      {/if}
    {/if}

    {#if pairIdentical !== null}
      <p
        class="pair-verdict"
        class:identical={pairIdentical}
        class:different={!pairIdentical}
      >
        {pairIdentical
          ? t("hash-inspector-pair-identical")
          : t("hash-inspector-pair-different")}
      </p>
    {/if}

    {#if rows.length === 0}
      <p class="hint">{t("hash-inspector-empty")}</p>
    {:else}
      <div class="rows" role="list">
        {#each rows as row (row.path)}
          {@const verdict = verdictFor(row)}
          <div class="row" role="listitem">
            <span class="name" title={row.path}>{row.path}</span>
            <span class="size">{formatBytes(row.size)}</span>
            {#if row.error}
              <code class="digest error">{row.error}</code>
            {:else}
              <code class="digest" title={row.hex}>{row.hex}</code>
            {/if}
            {#if verdict !== null}
              <span
                class="hash-verdict"
                class:identical={verdict}
                class:different={!verdict}
              >
                {verdict
                  ? t("collision-modal-hash-identical")
                  : t("collision-modal-hash-different")}
              </span>
            {/if}
          </div>
        {/each}
      </div>
    {/if}

    {#if truncated}
      <p class="hint bad">{t("hash-inspector-truncated", { rows: rows.length })}</p>
    {/if}

    <div class="actions">
      <button type="button" class="btn primary" onclick={closeHashInspector}>
        {t("hash-inspector-close")}
      </button>
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.35);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 120;
    padding: 16px;
  }
  .panel {
    display: flex;
    flex-direction: column;
    gap: 8px;
    width: 100%;
    max-width: 720px;
    max-height: 100%;
    background: var(--surface, #ffffff);
    border: 1px solid var(--border, rgba(0, 0, 0, 0.1));
    border-radius: 8px;
    padding: 16px;
    box-shadow: 0 16px 48px rgba(0, 0, 0, 0.3);
  }
  h2 {
    margin: 0;
    font-size: 14px;
  }
  .controls {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }
  .algo {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    color: var(--fg-dim, #6a6a6a);
  }
  .algo select {
    font: inherit;
    font-size: 12px;
    padding: 4px 6px;
    border-radius: 6px;
    border: 1px solid var(--border, rgba(0, 0, 0, 0.15));
    background: var(--surface, #ffffff);
    color: var(--fg, #1f1f1f);
  }
  .btn {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 5px 10px;
    border-radius: 6px;
    font: inherit;
    font-size: 12px;
    cursor: pointer;
    background: var(--hover, rgba(128, 128, 128, 0.12));
    border: 1px solid var(--border, rgba(128, 128, 128, 0.3));
    color: inherit;
  }
  .btn.primary {
    background: var(--accent, #4f8cff);
    border-color: transparent;
    color: #ffffff;
    font-weight: 600;
  }
  .btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .compare {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 11px;
    color: var(--fg-dim, #6a6a6a);
  }
  .compare input {
    font: inherit;
    font-size: 12px;
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    padding: 5px 8px;
    border-radius: 6px;
    border: 1px solid var(--border, rgba(0, 0, 0, 0.15));
    background: var(--surface, #ffffff);
    color: var(--fg, #1f1f1f);
  }
  .hint {
    margin: 0;
    font-size: 11px;
    color: var(--fg-dim, #6a6a6a);
    overflow-wrap: anywhere;
  }
  .hint.bad {
    color: var(--warn, #e4a040);
  }
  .pair-verdict {
    margin: 0;
    font-size: 12px;
    font-weight: 600;
  }
  .rows {
    flex: 1;
    min-height: 0;
    overflow: auto;
    border: 1px solid var(--border, rgba(0, 0, 0, 0.08));
    border-radius: 6px;
  }
  .row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto minmax(0, 1.2fr) auto;
    align-items: baseline;
    gap: 10px;
    padding: 5px 10px;
    font-size: 11px;
    border-bottom: 1px solid var(--border, rgba(0, 0, 0, 0.06));
  }
  .name {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--fg, #1f1f1f);
  }
  .size {
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
    color: var(--fg-dim, #6a6a6a);
  }
  .digest {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    user-select: all;
    color: var(--fg-dim, #6a6a6a);
  }
  .digest.error {
    color: var(--error, #d95757);
    user-select: text;
  }
  /* Same treatment as the collision dialog's quick-hash verdict. */
  .hash-verdict {
    white-space: nowrap;
    font-size: 12px;
    font-weight: 600;
  }
  .hash-verdict.identical,
  .pair-verdict.identical {
    color: var(--ok, #3faf6a);
  }
  .hash-verdict.different,
  .pair-verdict.different {
    color: var(--warn, #e4a040);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
  }
</style>
