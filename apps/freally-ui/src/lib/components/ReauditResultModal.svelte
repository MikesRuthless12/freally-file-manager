<!--
  FFM-M11 — verify-only re-audit result.

  Shown after "Re-verify" on a history row, or after a standalone
  source/destination re-audit from the footer. Headline verdict (clean
  vs drifted), the two roots and the algorithm used, then the bounded
  list of files that drifted. Matching files are counted, never listed.
-->
<script lang="ts">
  import { escapeToClose } from "../a11y";
  import { t } from "../i18n";
  import { closeReauditResult } from "../stores";
  import type { ReauditReport } from "../ipc";

  interface Props {
    report: ReauditReport;
  }

  let { report }: Props = $props();
  const clean = $derived(
    report.differs === 0 &&
      report.missing === 0 &&
      report.extra === 0 &&
      report.errors === 0,
  );

  function statusLabel(status: string): string {
    switch (status) {
      case "differs":
        return t("reaudit-status-differs");
      case "missing":
        return t("reaudit-status-missing");
      case "extra":
        return t("reaudit-status-extra");
      default:
        return t("reaudit-status-error");
    }
  }
</script>

<div
  class="backdrop"
  role="dialog"
  aria-modal="true"
  aria-labelledby="reaudit-title"
  tabindex="-1"
  use:escapeToClose={closeReauditResult}
>
  <div class="panel">
    <h2 id="reaudit-title" class:ok={clean} class:bad={!clean}>
      {clean ? t("reaudit-clean-title") : t("reaudit-drift-title")}
    </h2>
    <p class="roots" title="{report.srcRoot} → {report.dstRoot}">
      {report.srcRoot} → {report.dstRoot}
    </p>
    <p class="summary">
      {t("reaudit-summary", {
        ok: report.ok,
        differs: report.differs,
        missing: report.missing,
        extra: report.extra,
        errors: report.errors,
        algo: report.algo,
      })}
    </p>

    {#if report.rows.length > 0}
      <div class="rows" role="list">
        {#each report.rows as row (row.rel + row.status)}
          <div class="row" role="listitem">
            <span class="name" title={row.rel}>{row.rel}</span>
            {#if row.detail}
              <span class="detail" title={row.detail}>{row.detail}</span>
            {/if}
            <span class="status s-{row.status}">{statusLabel(row.status)}</span>
          </div>
        {/each}
      </div>
    {/if}

    <div class="actions">
      <button type="button" class="primary" onclick={closeReauditResult}>
        {t("reaudit-close")}
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
  h2.ok {
    color: var(--ok, #3faf6a);
  }
  h2.bad {
    color: var(--error, #d95757);
  }
  .roots,
  .summary {
    margin: 0;
    font-size: 11px;
    color: var(--fg-dim, #6a6a6a);
  }
  .roots {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .rows {
    flex: 1;
    min-height: 0;
    overflow: auto;
    border: 1px solid var(--border, rgba(0, 0, 0, 0.08));
    border-radius: 6px;
  }
  .row {
    display: flex;
    justify-content: space-between;
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
    flex: 1;
  }
  .detail {
    white-space: nowrap;
    color: var(--fg-dim, #6a6a6a);
  }
  .status {
    white-space: nowrap;
    font-weight: 600;
  }
  .s-differs {
    color: var(--error, #d95757);
  }
  .s-missing,
  .s-extra {
    color: var(--warn, #e4a040);
  }
  .s-error {
    color: var(--error, #d95757);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
  }
  .primary {
    padding: 6px 14px;
    border-radius: 6px;
    font-size: 12px;
    cursor: pointer;
    background: var(--accent, #4f8cff);
    color: #ffffff;
    border: 1px solid transparent;
    font-weight: 600;
  }
</style>
