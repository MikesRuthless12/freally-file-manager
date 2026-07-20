<!--
  Phase 9 — History drawer.

  Lives behind the Footer's "History" button. Lists every completed /
  running job recorded in the SQLite store, with a filter bar on top
  (kind, status, text search) and per-row actions (re-run + open
  detail + export CSV for the full set). Detail view is an inline
  sub-drawer showing the per-item rows.
-->
<script lang="ts">
  import { save } from "@tauri-apps/plugin-dialog";

  import Icon from "../icons/Icon.svelte";
  import { i18nVersion, t } from "../i18n";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import { formatBytes } from "../format";
  import {
    historyClearAll,
    historyExportCsv,
    historyItems,
    historyPurge,
    historyRerun,
    historySearch,
    mountSnapshot,
    undoPlan,
    jobFailedItems,
    exportFailedItems,
    startCopy,
    startMove,
  } from "../ipc";
  import {
    closeHistoryDetail,
    closeHistoryDrawer,
    historyDetailRow,
    historyDrawerOpen,
    openHistoryDetail,
    openUndoPreview,
    pushToast,
  } from "../stores";
  import type {
    HistoryFilterDto,
    HistoryItemDto,
    HistoryJobDto,
  } from "../types";

  let rows = $state<HistoryJobDto[]>([]);
  let items = $state<HistoryItemDto[]>([]);
  let busy = $state(false);
  let loadError = $state<string | null>(null);
  // Two-step confirm for "Clear all history" so a misclick can't
  // wipe months of records. First press flips this flag; a second
  // press within the same drawer session actually calls the IPC.
  let confirmingClearAll = $state(false);

  // Filter state — the refresh function re-queries when any changes.
  let kindFilter = $state<string>("");
  let statusFilter = $state<string>("");
  let textFilter = $state<string>("");

  async function refresh() {
    busy = true;
    loadError = null;
    try {
      const filter: HistoryFilterDto = {};
      if (kindFilter) filter.kind = kindFilter;
      if (statusFilter) filter.status = statusFilter;
      if (textFilter.trim()) filter.text = textFilter.trim();
      rows = await historySearch(filter);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      loadError = msg;
      if (msg === "history-unavailable") {
        // Leave row list empty; drawer shows the "unavailable"
        // banner below.
      } else {
        pushToast("error", msg);
      }
    } finally {
      busy = false;
    }
  }

  // Refresh when the drawer opens or any filter changes.
  $effect(() => {
    if ($historyDrawerOpen) {
      void refresh();
    }
  });

  // When detail row changes, fetch its items.
  $effect(() => {
    const id = $historyDetailRow;
    if (id !== null) {
      void loadItems(id);
    } else {
      items = [];
    }
  });

  async function loadItems(rowId: number) {
    try {
      items = await historyItems(rowId);
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  async function exportCsv() {
    const dest = await save({
      defaultPath: "freally-history.csv",
      filters: [{ name: "CSV", extensions: ["csv"] }],
    });
    if (!dest || typeof dest !== "string") return;
    try {
      await historyExportCsv(dest);
      pushToast("success", "toast-history-exported");
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  async function rerun(row: HistoryJobDto) {
    try {
      await historyRerun(row.rowId);
      pushToast("info", "toast-history-rerun-queued");
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  // FFM-M02 — fetch the undo plan for this job and open the preview
  // modal (rendered by App, above the drawer).
  async function undoRow(row: HistoryJobDto) {
    try {
      openUndoPreview(await undoPlan(row.rowId));
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  // FFM-M07 — re-enqueue only this job's failed files. Group by the
  // destination folder so each file lands back at its exact path
  // (a copy/move keeps the basename), then re-run through the same
  // verb the original job used (a failed move re-moves, not re-copies).
  function parentDir(p: string): string {
    const cut = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
    // Preserve the root when the only separator is the leading one
    // (e.g. "/f.txt" → "/", "C:\\x" handled by cut>2).
    if (cut < 0) return p;
    if (cut === 0) return p.slice(0, 1);
    return p.slice(0, cut);
  }

  async function retryFailed(row: HistoryJobDto) {
    try {
      const failed = await jobFailedItems(row.rowId);
      if (failed.length === 0) {
        pushToast("info", "toast-retry-failed-none");
        return;
      }
      const byDest = new Map<string, string[]>();
      for (const item of failed) {
        const dir = parentDir(item.dst);
        let arr = byDest.get(dir);
        if (!arr) byDest.set(dir, (arr = []));
        arr.push(item.src);
      }
      const run = row.kind === "move" ? startMove : startCopy;
      for (const [dir, srcs] of byDest) {
        await run(srcs, dir);
      }
      pushToast("success", t("toast-retry-failed-queued", { count: failed.length }));
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  // FFM-M07 — export this job's failed set (feeds an M13 --files-from
  // import too).
  async function exportFailed(row: HistoryJobDto) {
    try {
      const dest = await save({
        defaultPath: `freally-failed-${row.rowId}.csv`,
        filters: [
          { name: "CSV", extensions: ["csv"] },
          { name: "Text", extensions: ["txt"] },
          { name: "JSON", extensions: ["json"] },
        ],
      });
      if (!dest || typeof dest !== "string") return;
      const ext = dest.slice(dest.lastIndexOf(".") + 1).toLowerCase();
      const format = ext === "txt" || ext === "json" ? ext : "csv";
      await exportFailedItems(row.rowId, format, dest);
      pushToast("success", "toast-failed-exported");
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  async function mountRow(row: HistoryJobDto) {
    try {
      const picked = await openDialog({
        directory: true,
        multiple: false,
        title: t("mount-picker-title"),
      });
      if (picked == null || typeof picked !== "string") {
        return;
      }
      const dto = await mountSnapshot(row.rowId, picked);
      pushToast("success", t("mount-toast-mounted", { path: dto.mountpoint }));
    } catch (e) {
      const reason = e instanceof Error ? e.message : String(e);
      pushToast("error", t("mount-toast-failed", { reason }));
    }
  }

  async function purge30() {
    try {
      const n = await historyPurge(30);
      pushToast("info", t("toast-history-purged", { count: n }));
      await refresh();
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  async function clearAll() {
    if (!confirmingClearAll) {
      confirmingClearAll = true;
      // Auto-revert after 4 s so the "Confirm clear" button doesn't
      // sit armed forever.
      setTimeout(() => {
        confirmingClearAll = false;
      }, 4_000);
      return;
    }
    confirmingClearAll = false;
    try {
      const n = await historyClearAll();
      pushToast("info", t("toast-history-cleared", { count: n }));
      await refresh();
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  function fmtDate(ms: number | null): string {
    if (ms === null) return "—";
    try {
      return new Date(ms).toLocaleString();
    } catch {
      return String(ms);
    }
  }

  function fmtDuration(start: number, end: number | null): string {
    if (end === null) return "—";
    const ms = Math.max(0, end - start);
    if (ms < 1000) return t("duration-ms", { ms });
    const s = Math.floor(ms / 1000);
    if (s < 60) return t("duration-seconds", { s });
    const m = Math.floor(s / 60);
    const r = s % 60;
    return t("duration-minutes-seconds", { m, s: r });
  }

  // Map history wire-format `kind` / `status` strings to localized
  // labels. Unknown values fall back to the raw string so operator-
  // introduced kinds (e.g. a future "verify") still render.
  function localizedKind(kind: string): string {
    const key = `kind-${kind}`;
    const out = t(key);
    return out.startsWith("{") ? kind : out;
  }
  function localizedStatus(status: string): string {
    const key = `status-${status}`;
    const out = t(key);
    return out.startsWith("{") ? status : out;
  }
</script>

{#if $historyDrawerOpen}
  <aside class="drawer" aria-label={t("history-title")}>
    {#key $i18nVersion}
    <header>
      <h2>{t("history-title")}</h2>
      <button
        class="close"
        type="button"
        aria-label={t("action-close")}
        onclick={closeHistoryDrawer}
      >
        <Icon name="x" size={16} />
      </button>
    </header>

    <div class="filters">
      <label>
        {t("history-filter-kind")}
        <select bind:value={kindFilter}>
          <option value="">{t("history-filter-any")}</option>
          <option value="copy">{t("kind-copy")}</option>
          <option value="move">{t("kind-move")}</option>
        </select>
      </label>
      <label>
        {t("history-filter-status")}
        <select bind:value={statusFilter}>
          <option value="">{t("history-filter-any")}</option>
          <option value="running">{t("status-running")}</option>
          <option value="succeeded">{t("status-succeeded")}</option>
          <option value="failed">{t("status-failed")}</option>
          <option value="cancelled">{t("status-cancelled")}</option>
        </select>
      </label>
      <label class="text">
        {t("history-filter-text")}
        <input
          type="text"
          bind:value={textFilter}
          placeholder={t("history-search-placeholder")}
          onkeydown={(e) => {
            if (e.key === "Enter") void refresh();
          }}
        />
      </label>
      <button class="secondary" type="button" onclick={refresh} disabled={busy}>
        {t("history-refresh")}
      </button>
      <button
        class="secondary"
        type="button"
        onclick={exportCsv}
        disabled={busy || rows.length === 0}
      >
        {t("history-export-csv")}
      </button>
      <button class="secondary" type="button" onclick={purge30} disabled={busy}>
        {t("history-purge-30")}
      </button>
      <button
        class="secondary danger"
        type="button"
        onclick={clearAll}
        disabled={busy || rows.length === 0}
        title={t("history-clear-all-hint")}
      >
        {confirmingClearAll
          ? t("history-clear-all-confirm")
          : t("history-clear-all")}
      </button>
    </div>

    {#if loadError === "history-unavailable"}
      <p class="notice">{t("history-unavailable")}</p>
    {:else if rows.length === 0 && !busy}
      <p class="empty">{t("history-empty")}</p>
    {:else}
      <table>
        <thead>
          <tr>
            <th>{t("history-col-date")}</th>
            <th>{t("history-col-kind")}</th>
            <th>{t("history-col-src")}</th>
            <th>{t("history-col-dst")}</th>
            <th>{t("history-col-files")}</th>
            <th>{t("history-col-size")}</th>
            <th>{t("history-col-status")}</th>
            <th>{t("history-col-duration")}</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {#each rows as row (row.rowId)}
            <tr>
              <td>{fmtDate(row.startedAtMs)}</td>
              <td>{localizedKind(row.kind)}</td>
              <td class="path" title={row.srcRoot}>{row.srcRoot}</td>
              <td class="path" title={row.dstRoot}>{row.dstRoot}</td>
              <td>
                {row.filesOk}
                {#if row.filesFailed > 0}
                  <span class="failed">/ {row.filesFailed}</span>
                {/if}
              </td>
              <td>{formatBytes(row.totalBytes)}</td>
              <td>
                <span class="status" data-status={row.status}>
                  {localizedStatus(row.status)}
                </span>
              </td>
              <td>{fmtDuration(row.startedAtMs, row.finishedAtMs)}</td>
              <td class="actions">
                <button
                  type="button"
                  class="tiny"
                  onclick={() => openHistoryDetail(row.rowId)}
                  aria-label={t("history-detail-open")}
                >
                  <Icon name="info" size={13} />
                </button>
                <button
                  type="button"
                  class="tiny rerun-btn"
                  onclick={() => rerun(row)}
                  aria-label={t("history-rerun")}
                  title={t("history-rerun-hint")}
                  disabled={row.kind !== "copy" && row.kind !== "move"}
                >
                  <Icon name="refresh" size={13} />
                  <span>{t("history-rerun")}</span>
                </button>
                <button
                  type="button"
                  class="tiny"
                  onclick={() => mountRow(row)}
                  aria-label={t("mount-action-mount")}
                  title={t("mount-heading")}
                  disabled={row.status !== "succeeded"}
                >
                  <Icon name="folder" size={13} />
                  <span>{t("mount-action-mount")}</span>
                </button>
                <button
                  type="button"
                  class="tiny"
                  onclick={() => undoRow(row)}
                  aria-label={t("history-undo")}
                  title={t("history-undo-hint")}
                  disabled={row.status !== "succeeded" ||
                    (row.kind !== "copy" && row.kind !== "move")}
                >
                  <Icon name="rotate-ccw" size={13} />
                  <span>{t("history-undo")}</span>
                </button>
                {#if row.filesFailed > 0}
                  <button
                    type="button"
                    class="tiny"
                    onclick={() => retryFailed(row)}
                    aria-label={t("history-retry-failed")}
                    title={t("history-retry-failed-hint")}
                  >
                    <Icon name="refresh" size={13} />
                    <span>{t("history-retry-failed")}</span>
                  </button>
                  <button
                    type="button"
                    class="tiny"
                    onclick={() => exportFailed(row)}
                    aria-label={t("history-export-failed")}
                    title={t("history-export-failed-hint")}
                  >
                    <Icon name="download" size={13} />
                    <span>{t("history-export-failed")}</span>
                  </button>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
    {/key}
  </aside>

  {#if $historyDetailRow !== null}
    <aside class="detail" aria-label={t("history-detail-title")}>
      {#key $i18nVersion}
      <header>
        <h3>{t("history-detail-title")}</h3>
        <button
          class="close"
          type="button"
          aria-label={t("action-close")}
          onclick={closeHistoryDetail}
        >
          <Icon name="x" size={16} />
        </button>
      </header>
      {#if items.length === 0}
        <p class="empty">{t("history-detail-empty")}</p>
      {:else}
        <table>
          <thead>
            <tr>
              <th>{t("history-col-src")}</th>
              <th>{t("history-col-size")}</th>
              <th>{t("history-col-status")}</th>
              <th>{t("history-col-error")}</th>
            </tr>
          </thead>
          <tbody>
            {#each items as it}
              <tr>
                <td class="path" title={it.src}>{it.src}</td>
                <td>{formatBytes(it.size)}</td>
                <td>
                  <span class="status" data-status={it.status}>
                    {localizedStatus(it.status)}
                  </span>
                </td>
                <td class="err">{it.errorMsg ?? "—"}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
      {/key}
    </aside>
  {/if}
{/if}

<style>
  .drawer {
    position: fixed;
    inset: 0;
    background: var(--surface, #ffffff);
    color: var(--fg, #1f1f1f);
    display: flex;
    flex-direction: column;
    z-index: 86;
  }

  .detail {
    position: fixed;
    inset: 0;
    background: var(--surface, #ffffff);
    color: var(--fg, #1f1f1f);
    display: flex;
    flex-direction: column;
    z-index: 87;
  }

  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 16px;
    border-bottom: 1px solid var(--border, rgba(128, 128, 128, 0.3));
  }

  h2,
  h3 {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
  }

  .close {
    background: transparent;
    border: 1px solid transparent;
    color: inherit;
    padding: 4px;
    border-radius: 4px;
    cursor: pointer;
  }

  .filters {
    display: flex;
    align-items: end;
    gap: 8px;
    padding: 8px 16px;
    border-bottom: 1px solid var(--border, rgba(128, 128, 128, 0.3));
    flex-wrap: wrap;
  }

  .filters label {
    display: flex;
    flex-direction: column;
    gap: 3px;
    font-size: 11px;
    color: var(--muted, #6a6a6a);
  }

  .filters label.text {
    flex: 1;
    min-width: 140px;
  }

  .filters select,
  .filters input {
    padding: 4px 6px;
    font-size: 12px;
    border: 1px solid var(--border, rgba(128, 128, 128, 0.3));
    border-radius: 4px;
    background: var(--surface, #ffffff);
    color: inherit;
  }

  button {
    font-size: 12px;
    padding: 4px 10px;
    border-radius: 4px;
    border: 1px solid var(--border, rgba(128, 128, 128, 0.3));
    background: var(--surface-alt, rgba(0, 0, 0, 0.04));
    color: inherit;
    cursor: pointer;
  }

  button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  button.secondary.danger {
    color: var(--error, #c24141);
    border-color: color-mix(in srgb, var(--error, #c24141) 40%, transparent);
  }

  button.secondary.danger:hover:not(:disabled) {
    background: color-mix(in srgb, var(--error, #c24141) 10%, transparent);
    border-color: var(--error, #c24141);
  }

  button.tiny {
    padding: 3px 4px;
    background: transparent;
    border: 1px solid transparent;
  }

  button.tiny:hover:not(:disabled) {
    background: var(--surface-alt, rgba(0, 0, 0, 0.04));
  }

  button.rerun-btn {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 3px 8px;
    border: 1px solid var(--border, rgba(128, 128, 128, 0.3));
    border-radius: 4px;
    font-size: 11.5px;
    color: inherit;
  }

  button.rerun-btn:hover:not(:disabled) {
    background: var(--row-selected, rgba(79, 140, 255, 0.12));
    border-color: var(--accent, #4f8cff);
  }

  .notice,
  .empty {
    padding: 24px 16px;
    color: var(--muted, #6a6a6a);
    font-size: 13px;
    text-align: center;
  }

  table {
    flex: 1;
    width: 100%;
    border-collapse: collapse;
    font-size: 11.5px;
    overflow-y: auto;
    display: block;
  }

  thead {
    position: sticky;
    top: 0;
    background: var(--surface, #ffffff);
    z-index: 1;
  }

  th,
  td {
    text-align: left;
    padding: 5px 10px;
    border-bottom: 1px solid var(--border, rgba(128, 128, 128, 0.15));
    vertical-align: top;
  }

  td.path {
    font-family: var(--mono, ui-monospace, SFMono-Regular, Menlo, monospace);
    font-size: 10.5px;
    max-width: 180px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  td.err {
    color: var(--error, #c24141);
    font-size: 10.5px;
  }

  td.actions {
    white-space: nowrap;
    display: flex;
    gap: 4px;
  }

  .failed {
    color: var(--error, #c24141);
  }

  .status {
    font-size: 10px;
    padding: 2px 6px;
    border-radius: 3px;
    background: var(--surface-alt, rgba(0, 0, 0, 0.04));
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .status[data-status="succeeded"],
  .status[data-status="ok"] {
    background: rgba(63, 175, 106, 0.16);
    color: var(--ok, #2e7a4a);
  }

  .status[data-status="failed"] {
    background: rgba(217, 87, 87, 0.16);
    color: var(--error, #c24141);
  }

  .status[data-status="cancelled"] {
    background: rgba(228, 160, 64, 0.16);
    color: var(--warn, #b87b1e);
  }
</style>
