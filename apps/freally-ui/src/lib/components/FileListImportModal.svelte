<!--
  FFM-M13 — file-list import as a copy source.

  Opened from the Header's "Import list…" action. Shows what the
  manifest resolved to (found vs missing), lets the user choose whether
  the copy preserves each file's structure relative to a root or lands
  flat, then picks a destination and enqueues.

  The engine takes one destination root per job, so a structured import
  enqueues one job per relative directory — the grouping is planned on
  the Rust side (`filelist_plan`) so the rule is tested in one place.
-->
<script lang="ts">
  import { untrack } from "svelte";

  import { t } from "../i18n";
  import { closeFileListImport, pushToast } from "../stores";
  import {
    filelistPlan,
    pickFolders,
    startCopy,
    startMove,
    type FileListDto,
  } from "../ipc";

  interface Props {
    list: FileListDto;
    /** Manifest path, shown so the user can confirm what was read. */
    manifest: string;
  }

  let { list, manifest }: Props = $props();

  let kind = $state<"copy" | "move">("copy");
  // Seeded once from the manifest's common ancestor, then owned by the
  // user. The modal is mounted fresh per import, so an initial capture
  // is the whole story — `untrack` says so explicitly.
  let preserveStructure = $state(untrack(() => list.commonRoot !== null));
  let relativeRoot = $state(untrack(() => list.commonRoot ?? ""));
  let destination = $state<string | null>(null);
  let busy = $state(false);

  async function pickDestination() {
    try {
      const picked = await pickFolders();
      if (picked.length > 0) destination = picked[0];
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  async function start() {
    if (!destination || busy || list.paths.length === 0) return;
    busy = true;
    try {
      const groups = await filelistPlan(
        list.paths,
        preserveStructure ? relativeRoot : "",
      );
      const run = kind === "move" ? startMove : startCopy;
      for (const group of groups) {
        const dst = group.relDir
          ? `${destination}/${group.relDir}`
          : destination;
        await run(group.sources, dst);
      }
      pushToast(
        "success",
        t("filelist-toast-queued", { files: list.paths.length }),
      );
      closeFileListImport();
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    } finally {
      busy = false;
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") closeFileListImport();
  }
</script>

<div
  class="backdrop"
  role="dialog"
  aria-modal="true"
  aria-labelledby="filelist-title"
  tabindex="-1"
  onkeydown={onKeydown}
>
  <div class="panel">
    <h2 id="filelist-title">{t("filelist-title")}</h2>
    <p class="sub" title={manifest}>{manifest}</p>
    <p class="sub">
      {t("filelist-summary", {
        found: list.paths.length,
        missing: list.missing.length,
      })}
    </p>

    {#if list.missing.length > 0}
      <details class="missing">
        <summary>{t("filelist-missing-header", { count: list.missing.length })}</summary>
        <div class="rows" role="list">
          {#each list.missing as p (p)}
            <div class="row" role="listitem" title={p}>{p}</div>
          {/each}
        </div>
      </details>
    {/if}

    <div class="kinds" role="radiogroup" aria-label={t("filelist-title")}>
      <label>
        <input type="radio" name="filelist-kind" value="copy" bind:group={kind} />
        {t("drop-dialog-copy")}
      </label>
      <label>
        <input type="radio" name="filelist-kind" value="move" bind:group={kind} />
        {t("drop-dialog-move")}
      </label>
    </div>

    <label class="structure">
      <input type="checkbox" bind:checked={preserveStructure} />
      {t("filelist-preserve-structure")}
    </label>
    {#if preserveStructure}
      <input
        class="root"
        type="text"
        spellcheck="false"
        placeholder={t("filelist-relative-root-placeholder")}
        bind:value={relativeRoot}
      />
    {/if}

    <div class="dest">
      <button type="button" class="btn" onclick={pickDestination} disabled={busy}>
        {destination
          ? t("drop-dialog-change-destination")
          : t("drop-dialog-pick-destination")}
      </button>
      {#if destination}
        <span class="path" title={destination}>{destination}</span>
      {/if}
    </div>

    <div class="actions">
      <button type="button" class="btn" onclick={closeFileListImport} disabled={busy}>
        {t("action-cancel")}
      </button>
      <button
        type="button"
        class="btn primary"
        onclick={start}
        disabled={busy || !destination || list.paths.length === 0}
      >
        {kind === "copy"
          ? t("drop-dialog-start-copy")
          : t("drop-dialog-start-move")}
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
    max-width: 560px;
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
  .sub {
    margin: 0;
    font-size: 11px;
    color: var(--fg-dim, #6a6a6a);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .missing > summary {
    font-size: 11px;
    color: var(--warn, #b7791f);
    cursor: pointer;
  }
  .rows {
    max-height: 140px;
    overflow: auto;
    border: 1px solid var(--border, rgba(0, 0, 0, 0.08));
    border-radius: 6px;
    margin-top: 4px;
  }
  .row {
    padding: 3px 8px;
    font-size: 11px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    border-bottom: 1px solid var(--border, rgba(0, 0, 0, 0.06));
  }
  .kinds,
  .structure,
  .dest {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 12px;
  }
  .root {
    font: inherit;
    font-size: 12px;
    padding: 5px 8px;
    border-radius: 6px;
    border: 1px solid var(--border, rgba(0, 0, 0, 0.15));
    background: var(--surface, #ffffff);
    color: var(--fg, #1f1f1f);
  }
  .path {
    font-size: 11px;
    color: var(--fg-dim, #6a6a6a);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 6px;
  }
  .btn {
    padding: 5px 12px;
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
    opacity: 0.45;
    cursor: not-allowed;
  }
</style>
