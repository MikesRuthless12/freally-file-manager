<!--
  FFM-M08 — checksum sidecar verify result.

  Shown after a sidecar (.sha256/.md5/.b3/.sfv/…) is dragged onto the
  window: a headline verdict (all-good vs failures) plus the bounded
  list of files that failed or went missing.
-->
<script lang="ts">
  import { t } from "../i18n";
  import { closeSidecarVerify } from "../stores";
  import type { SidecarVerifyReport } from "../ipc";

  interface Props {
    report: SidecarVerifyReport;
  }

  let { report }: Props = $props();
  const clean = $derived(report.failed === 0 && report.missing === 0);

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") closeSidecarVerify();
  }
</script>

<div
  class="backdrop"
  role="dialog"
  aria-modal="true"
  aria-labelledby="sidecar-title"
  tabindex="-1"
  onkeydown={onKeydown}
>
  <div class="panel">
    <h2 id="sidecar-title" class:ok={clean} class:bad={!clean}>
      {clean ? t("sidecar-verify-clean-title") : t("sidecar-verify-bad-title")}
    </h2>
    <p class="summary">
      {t("sidecar-verify-summary", {
        ok: report.ok,
        failed: report.failed,
        missing: report.missing,
      })}
    </p>

    {#if report.problems.length > 0}
      <div class="rows" role="list">
        {#each report.problems as row, i (i)}
          <div class="row" role="listitem">
            <span class="name" title={row.path}>{row.path}</span>
            <span class="status s-{row.status}">
              {row.status === "missing"
                ? t("sidecar-verify-missing")
                : t("sidecar-verify-failed")}
            </span>
          </div>
        {/each}
      </div>
    {/if}

    <div class="actions">
      <button type="button" class="primary" onclick={closeSidecarVerify}>
        {t("sidecar-verify-close")}
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
    max-width: 520px;
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
  }
  .status {
    white-space: nowrap;
    font-weight: 600;
  }
  .s-failed {
    color: var(--error, #d95757);
  }
  .s-missing {
    color: var(--warn, #e4a040);
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
