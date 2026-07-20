<!--
  Phase 49d — Restore browser modal.

  Lists a snapshot's files (checkbox + size, select-all), picks a
  destination, and restores. On existing-file collisions it shows a
  batch conflict step using the engine's resolution vocabulary
  (overwrite / skip / keep-both) before writing anything.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import { t } from "../i18n";
  import { formatBytes } from "../format";
  import {
    restorePaths,
    restorePreview,
    snapshotTree,
    type SnapshotFileDto,
  } from "../ipc";
  import { pushToast } from "../stores";

  let {
    snapshotId,
    onClose,
    refresh,
  }: {
    snapshotId: number;
    onClose: () => void;
    refresh: () => void | Promise<void>;
  } = $props();

  let files = $state<SnapshotFileDto[]>([]);
  let selected = $state<Set<string>>(new Set());
  let dst = $state("");
  let loading = $state(true);
  let busy = $state(false);
  // null = picking files; a number = the conflict step (that many exist).
  let conflictCount = $state<number | null>(null);

  onMount(async () => {
    try {
      files = await snapshotTree(snapshotId);
      selected = new Set(files.map((f) => f.path)); // select-all by default
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    } finally {
      loading = false;
    }
  });

  const allSelected = $derived(
    files.length > 0 && selected.size === files.length,
  );

  function toggle(path: string) {
    const next = new Set(selected);
    if (next.has(path)) next.delete(path);
    else next.add(path);
    selected = next;
  }

  function toggleAll() {
    selected = allSelected ? new Set() : new Set(files.map((f) => f.path));
  }

  async function pickDest() {
    const picked = await openDialog({ directory: true, multiple: false });
    if (typeof picked === "string") dst = picked;
  }

  async function startRestore() {
    if (!dst || selected.size === 0) return;
    busy = true;
    try {
      const preview = await restorePreview(snapshotId, [...selected], dst);
      const conflicts = preview.filter((p) => p.exists).length;
      if (conflicts > 0) {
        conflictCount = conflicts; // show the conflict step
        return;
      }
      await doRestore("overwrite");
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    } finally {
      busy = false;
    }
  }

  async function doRestore(conflict: "overwrite" | "skip" | "keep-both") {
    busy = true;
    try {
      const rep = await restorePaths(snapshotId, [...selected], dst, conflict);
      if (rep.failed > 0) {
        // Don't hide partial failures behind a green success toast.
        pushToast(
          "error",
          t("restore-toast-partial", {
            restored: rep.restored,
            skipped: rep.skipped,
            failed: rep.failed,
          }),
        );
      } else {
        pushToast(
          "success",
          t("restore-toast-done", {
            restored: rep.restored,
            skipped: rep.skipped,
          }),
        );
      }
      await refresh();
      onClose();
    } catch (e) {
      pushToast(
        "error",
        t("restore-toast-failed", {
          reason: e instanceof Error ? e.message : String(e),
        }),
      );
    } finally {
      busy = false;
    }
  }
</script>

<div class="overlay backdrop" role="dialog" aria-modal="true" aria-label={t("restore-title")}>
  <div class="modal">
    <header>
      <h3>{t("restore-title")}</h3>
      <button class="x" type="button" onclick={onClose} aria-label="close">✕</button>
    </header>

    {#if loading}
      <p class="empty">…</p>
    {:else if files.length === 0}
      <p class="empty">{t("restore-empty")}</p>
    {:else if conflictCount !== null}
      <div class="conflict">
        <p>{t("restore-conflict-body", { count: conflictCount })}</p>
        <div class="conflict-actions">
          <button type="button" disabled={busy} onclick={() => doRestore("overwrite")}>
            {t("restore-conflict-overwrite")}
          </button>
          <button type="button" disabled={busy} onclick={() => doRestore("skip")}>
            {t("restore-conflict-skip")}
          </button>
          <button type="button" disabled={busy} onclick={() => doRestore("keep-both")}>
            {t("restore-conflict-keep-both")}
          </button>
        </div>
      </div>
    {:else}
      <div class="selbar">
        <label>
          <input type="checkbox" checked={allSelected} onchange={toggleAll} />
          {t("restore-select-all")}
        </label>
      </div>
      <ul class="files">
        {#each files as f (f.path)}
          <li>
            <label>
              <input
                type="checkbox"
                checked={selected.has(f.path)}
                onchange={() => toggle(f.path)}
              />
              <span class="p">{f.path}</span>
              <span class="s">{formatBytes(f.size)}</span>
            </label>
          </li>
        {/each}
      </ul>
      <div class="dest">
        <span class="lbl">{t("restore-dest")}</span>
        <input type="text" readonly value={dst} placeholder="…" />
        <button type="button" onclick={pickDest}>…</button>
      </div>
      <footer>
        <button
          class="primary"
          type="button"
          disabled={busy || !dst || selected.size === 0}
          onclick={startRestore}
        >
          {t("restore-confirm", { n: selected.size })}
        </button>
      </footer>
    {/if}
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.45);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 95;
  }
  .modal {
    background: var(--surface, #ffffff);
    color: var(--fg, #1f1f1f);
    width: min(560px, 92vw);
    max-height: 84vh;
    border-radius: 10px;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    box-shadow: 0 12px 48px rgba(0, 0, 0, 0.35);
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 16px;
    border-bottom: 1px solid var(--border, rgba(128, 128, 128, 0.3));
  }
  h3 {
    margin: 0;
    font-size: 1rem;
  }
  .x {
    background: none;
    border: none;
    cursor: pointer;
    color: inherit;
    font-size: 1rem;
  }
  .selbar {
    padding: 8px 16px;
    border-bottom: 1px solid var(--border, rgba(128, 128, 128, 0.2));
  }
  .files {
    list-style: none;
    margin: 0;
    padding: 8px 16px;
    overflow-y: auto;
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .files label {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .files .p {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .files .s {
    color: var(--muted, #666666);
    font-size: 0.8rem;
    flex-shrink: 0;
  }
  .dest {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px 16px;
    border-top: 1px solid var(--border, rgba(128, 128, 128, 0.2));
  }
  .dest input {
    flex: 1;
    padding: 6px 8px;
  }
  .dest .lbl {
    color: var(--muted, #666666);
    font-size: 0.85rem;
  }
  footer {
    padding: 12px 16px;
    display: flex;
    justify-content: flex-end;
    border-top: 1px solid var(--border, rgba(128, 128, 128, 0.2));
  }
  .primary {
    background: var(--accent, #3b82f6);
    color: #ffffff;
    border: none;
    padding: 8px 16px;
    border-radius: 6px;
    cursor: pointer;
  }
  .primary:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .conflict {
    padding: 20px 16px;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .conflict-actions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .conflict-actions button {
    padding: 8px 14px;
    cursor: pointer;
  }
  .empty {
    padding: 32px 16px;
    text-align: center;
    color: var(--muted, #666666);
  }
</style>
