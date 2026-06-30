<!--
  Phase 49c — Backup sources sub-view, slotted into the Library drawer.

  Lists the folders the user designated for backup, lets them add one
  (native folder picker + optional comma-separated exclude globs), run
  "Back up now" (snapshots into the repository as a Backup), and remove
  sources. A live progress bar + toasts are driven by the `backup-*`
  events; on completion the parent Library `refresh` runs so the new
  snapshot + the updated dedup hero appear immediately.
-->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import { t } from "../i18n";
  import {
    backupNow,
    sourcesAdd,
    sourcesList,
    sourcesRemove,
    type SourceConfigDto,
  } from "../ipc";
  import { pushToast } from "../stores";

  let { refresh }: { refresh: () => void | Promise<void> } = $props();

  type Progress = { filesDone: number; filesTotal: number };

  let sources = $state<SourceConfigDto[]>([]);
  let progress = $state<Record<string, Progress | undefined>>({});
  let newExcludes = $state("");
  let unlistenFns: UnlistenFn[] = [];

  async function reload() {
    try {
      sources = await sourcesList();
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  function clearProgress(id: string) {
    const next = { ...progress };
    delete next[id];
    progress = next;
  }

  onMount(() => {
    void reload();
    (async () => {
      unlistenFns.push(
        await listen<{ id: string; filesDone: number; filesTotal: number }>(
          "backup-progress",
          (evt) => {
            progress = {
              ...progress,
              [evt.payload.id]: {
                filesDone: evt.payload.filesDone,
                filesTotal: evt.payload.filesTotal,
              },
            };
          },
        ),
      );
      unlistenFns.push(
        await listen<{ id: string; files: number }>(
          "backup-completed",
          (evt) => {
            clearProgress(evt.payload.id);
            pushToast(
              "success",
              t("backup-toast-completed", {
                label: labelOf(evt.payload.id),
                files: evt.payload.files,
              }),
            );
            void reload();
            void refresh();
          },
        ),
      );
      // Failures clear the progress bar here; the user-facing toast is
      // raised by `runBackup`'s catch so there is exactly one per failure.
      unlistenFns.push(
        await listen<{ id: string }>("backup-failed", (evt) => {
          clearProgress(evt.payload.id);
        }),
      );
    })().catch((e) => console.error("[backup-listen]", e));
  });

  onDestroy(() => {
    for (const f of unlistenFns) {
      try {
        f();
      } catch {
        // Unlisten errors are best-effort; swallow.
      }
    }
  });

  function labelOf(id: string): string {
    return sources.find((s) => s.id === id)?.label ?? id;
  }

  function baseName(path: string): string {
    const parts = path.split(/[\\/]/).filter(Boolean);
    return parts.length ? parts[parts.length - 1] : path;
  }

  async function addSource() {
    const picked = await openDialog({ directory: true, multiple: false });
    if (typeof picked !== "string") return;
    const globs = newExcludes
      .split(",")
      .map((g) => g.trim())
      .filter(Boolean);
    try {
      await sourcesAdd(baseName(picked), picked, globs);
      newExcludes = "";
      await reload();
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  async function runBackup(id: string) {
    pushToast("info", t("backup-toast-started", { label: labelOf(id) }));
    try {
      await backupNow(id);
    } catch (e) {
      const reason = e instanceof Error ? e.message : String(e);
      pushToast("error", t("backup-toast-failed", { label: labelOf(id), reason }));
    }
  }

  async function removeSource(id: string) {
    try {
      await sourcesRemove(id);
      await reload();
      void refresh();
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  function fmtDate(iso: string): string {
    if (!iso) return "";
    const d = new Date(iso);
    return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
  }
</script>

<div class="add-row">
  <input
    type="text"
    placeholder={t("backup-exclude-ph")}
    bind:value={newExcludes}
  />
  <button type="button" onclick={addSource}>{t("backup-add-source")}</button>
</div>

{#if sources.length === 0}
  <p class="empty">{t("backup-empty")}</p>
{:else}
  <ul class="list">
    {#each sources as s (s.id)}
      <li>
        <div class="row">
          <div class="info">
            <span class="label">{s.label}</span>
            <span class="meta" title={s.path}>{s.path}</span>
            <span class="meta">
              {#if s.lastRunAt}
                {t("backup-last-run", { when: fmtDate(s.lastRunAt) })}
              {:else}
                {t("backup-never-run")}
              {/if}
            </span>
          </div>
          <div class="actions">
            <button
              type="button"
              disabled={!!progress[s.id]}
              onclick={() => runBackup(s.id)}
            >
              {t("backup-now")}
            </button>
            <button
              type="button"
              class="danger"
              disabled={!!progress[s.id]}
              onclick={() => removeSource(s.id)}
            >
              {t("backup-remove")}
            </button>
          </div>
        </div>
        {#if progress[s.id]}
          {@const p = progress[s.id]}
          <div class="progress">
            <div
              class="bar"
              style:width={p && p.filesTotal > 0
                ? `${Math.round((p.filesDone / p.filesTotal) * 100)}%`
                : "10%"}
            ></div>
          </div>
          <span class="meta">
            {t("backup-running", { files: p ? p.filesTotal : 0 })}
          </span>
        {/if}
      </li>
    {/each}
  </ul>
{/if}

<style>
  .add-row {
    display: flex;
    gap: 8px;
    padding: 12px 16px;
  }
  .add-row input {
    flex: 1;
    padding: 6px 8px;
  }
  .list {
    list-style: none;
    margin: 0;
    padding: 8px 16px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .list li {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 8px;
    border: 1px solid var(--border, rgba(128, 128, 128, 0.2));
    border-radius: 6px;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }
  .info {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .label {
    font-weight: 600;
  }
  .meta {
    color: var(--muted, #666666);
    font-size: 0.8rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .actions {
    display: flex;
    gap: 6px;
    flex-shrink: 0;
  }
  .danger {
    color: var(--danger, #dc2626);
  }
  .progress {
    height: 6px;
    background: var(--border, rgba(128, 128, 128, 0.2));
    border-radius: 3px;
    overflow: hidden;
  }
  .bar {
    height: 100%;
    background: var(--accent, #3b82f6);
    transition: width 0.2s ease;
  }
  .empty {
    padding: 24px 16px;
    color: var(--muted, #666666);
    text-align: center;
  }
</style>
