<!--
  FFM-M02 — undo preview. Shows exactly what undoing a completed copy
  or move will touch before anything happens: per-row action (trash the
  copied destination / move the file back) and per-row status (ready,
  skipped because the file changed or vanished, or conflicted because
  the original path is occupied again). Confirm applies only the ready
  rows; every removal goes to the OS trash, never a permanent unlink.
-->
<script lang="ts">
  import { t } from "../i18n";
  import { undoApply } from "../ipc";
  import { closeUndoPreview, pushToast } from "../stores";
  import { formatBytes } from "../format";
  import type { UndoPlanDto, UndoRowDto } from "../types";

  interface Props {
    plan: UndoPlanDto;
  }

  let { plan }: Props = $props();
  let busy = $state(false);

  function rowName(row: UndoRowDto): string {
    const p = row.dst;
    const cut = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
    return cut >= 0 ? p.slice(cut + 1) : p;
  }

  function statusKey(row: UndoRowDto): string {
    return `undo-status-${row.status}`;
  }

  async function confirm() {
    if (busy || plan.ready === 0) return;
    busy = true;
    try {
      const report = await undoApply(plan.jobId);
      pushToast(
        report.failed > 0 ? "error" : "success",
        t("toast-undo-done", {
          done: report.done,
          skipped: report.skipped,
          failed: report.failed,
        }),
      );
      closeUndoPreview();
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
      busy = false;
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape" && !busy) closeUndoPreview();
  }
</script>

<div
  class="backdrop"
  role="dialog"
  aria-modal="true"
  aria-labelledby="undo-title"
  tabindex="-1"
  onkeydown={onKeydown}
>
  <div class="panel">
    <h2 id="undo-title">
      {plan.kind === "move" ? t("undo-title-move") : t("undo-title-copy")}
    </h2>
    <p class="summary">
      {t("undo-summary", { ready: plan.ready, total: plan.rows.length })}
    </p>

    <div class="rows" role="list">
      {#each plan.rows as row, i (i)}
        <div class="row" role="listitem" class:dim={row.status !== "ready"}>
          <span class="name" title={row.action === "trash-dst" ? row.dst : `${row.dst} → ${row.src}`}>
            {rowName(row)}
          </span>
          <span class="action">
            {row.action === "trash-dst"
              ? t("undo-action-trash")
              : t("undo-action-move-back")}
          </span>
          <span class="size">{formatBytes(row.size)}</span>
          <span class="status s-{row.status}">{t(statusKey(row))}</span>
        </div>
      {/each}
    </div>

    <div class="actions">
      <button type="button" class="secondary" disabled={busy} onclick={closeUndoPreview}>
        {t("undo-cancel")}
      </button>
      <button
        type="button"
        class="primary"
        disabled={busy || plan.ready === 0}
        onclick={confirm}
      >
        {t("undo-confirm", { count: plan.ready })}
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
    color: var(--fg-strong, #1f1f1f);
  }
  .summary {
    margin: 0;
    font-size: 11px;
    color: var(--fg-dim, #6a6a6a);
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
    grid-template-columns: 1fr auto auto auto;
    gap: 10px;
    align-items: center;
    padding: 5px 10px;
    font-size: 11px;
    border-bottom: 1px solid var(--border, rgba(0, 0, 0, 0.06));
  }
  .row.dim {
    opacity: 0.55;
  }
  .name {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--fg, #1f1f1f);
  }
  .action,
  .size {
    color: var(--fg-dim, #6a6a6a);
    white-space: nowrap;
  }
  .status {
    white-space: nowrap;
    font-weight: 600;
  }
  .s-ready {
    color: var(--ok, #3faf6a);
  }
  .s-skip-missing,
  .s-skip-changed {
    color: var(--warn, #e4a040);
  }
  .s-conflict {
    color: var(--error, #d95757);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
  .actions button {
    padding: 6px 14px;
    border-radius: 6px;
    font-size: 12px;
    cursor: pointer;
  }
  .secondary {
    background: transparent;
    color: var(--fg, #1f1f1f);
    border: 1px solid var(--border, rgba(0, 0, 0, 0.1));
  }
  .primary {
    background: var(--accent, #4f8cff);
    color: #ffffff;
    border: 1px solid transparent;
    font-weight: 600;
  }
  .primary:disabled,
  .secondary:disabled {
    opacity: 0.4;
    cursor: default;
  }
</style>
