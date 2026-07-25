<!--
  FFM-M12 — cross-filesystem filename doctor.

  A preflight strip that scans the source names for anything the
  destination filesystem cannot hold — Windows reserved device names,
  illegal characters, trailing dots/spaces, component/path length
  overflow, case-fold collisions — and offers the rename plan that
  fixes them. Silent when the tree is clean, so it costs the common
  case nothing but the scan.

  Mounted inside the pre-copy gates (the drop-staging dialog and the
  tree-diff preview) rather than owning a modal of its own.
-->
<script lang="ts">
  import { t } from "../i18n";
  import { pushToast } from "../stores";
  import {
    filenameDoctorApply,
    filenameDoctorScan,
    type DoctorReport,
    type DoctorTarget,
  } from "../ipc";

  interface Props {
    /** Source root to scan. */
    src: string;
    /** Where the files will land — path length is judged against it. */
    dstRoot: string;
    /**
     * Which filesystem's rules to check against.
     *
     * Defaults to `windows` — the strictest set — deliberately, NOT to
     * `auto`. `auto` resolves to the *host* OS, which is the one thing
     * this panel must not assume: on a Linux host copying to an
     * exFAT/NTFS stick it skips the reserved-name, illegal-character,
     * trailing-dot and case-collision checks entirely, so a tree full
     * of names the destination cannot hold reports zero findings and
     * the copy fails later. Over-flagging is per-row declinable;
     * under-flagging is silent. On a Windows host this is what `auto`
     * already resolved to, so nothing changes there.
     */
    target?: DoctorTarget;
  }

  let { src, dstRoot, target = "windows" }: Props = $props();

  let report = $state<DoctorReport | null>(null);
  let busy = $state(false);
  /** Rows the user has excluded from the rename plan, keyed by `rel`. */
  let declined = $state<Record<string, boolean>>({});

  const fixable = $derived(
    (report?.rows ?? []).filter((r) => r.suggested !== null && !declined[r.rel]),
  );

  function issueLabel(issue: string): string {
    switch (issue) {
      case "reserved-name":
        return t("doctor-issue-reserved-name");
      case "illegal-char":
        return t("doctor-issue-illegal-char");
      case "trailing-dot-space":
        return t("doctor-issue-trailing-dot-space");
      case "component-too-long":
        return t("doctor-issue-component-too-long");
      case "path-too-long":
        return t("doctor-issue-path-too-long");
      default:
        return t("doctor-issue-case-collision");
    }
  }

  $effect(() => {
    const s = src;
    const d = dstRoot;
    const tgt = target;
    if (!s || !d) return;
    let cancelled = false;
    void (async () => {
      try {
        const dto = await filenameDoctorScan(s, d, tgt);
        if (!cancelled) {
          report = dto;
          declined = {};
        }
      } catch {
        // A scan failure must never block the copy the user asked for;
        // the strip simply stays hidden.
        if (!cancelled) report = null;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  async function applyPlan() {
    if (fixable.length === 0 || busy) return;
    busy = true;
    try {
      const renames = fixable.map(
        (r) => [r.rel, r.suggested as string] as [string, string],
      );
      const result = await filenameDoctorApply(src, renames);
      pushToast(
        result.failed > 0 ? "error" : "success",
        t("doctor-toast-applied", {
          renamed: result.renamed,
          failed: result.failed,
        }),
      );
      // Re-scan so the strip reflects what is left (and picks up any
      // collision the renames themselves resolved).
      report = await filenameDoctorScan(src, dstRoot, target);
      declined = {};
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    } finally {
      busy = false;
    }
  }
</script>

{#if report && report.rows.length > 0}
  <section class="doctor" aria-label={t("doctor-title")}>
    <header>
      <strong>{t("doctor-title")}</strong>
      <span class="sub">
        {t("doctor-summary", {
          flagged: report.rows.length,
          scanned: report.scanned,
          target: report.target,
        })}
      </span>
    </header>

    <div class="rows" role="list">
      {#each report.rows as row (row.rel)}
        <div class="row" role="listitem">
          {#if row.suggested}
            <input
              type="checkbox"
              checked={!declined[row.rel]}
              aria-label={row.rel}
              onchange={(e) => {
                declined = {
                  ...declined,
                  [row.rel]: !(e.currentTarget as HTMLInputElement).checked,
                };
              }}
            />
          {:else}
            <span class="no-fix" aria-hidden="true">—</span>
          {/if}
          <span class="name" title={row.rel}>{row.rel}</span>
          <span class="issues">
            {row.issues.map(issueLabel).join(", ")}
          </span>
          {#if row.suggested}
            <span class="suggested" title={row.suggested}>→ {row.suggested}</span>
          {:else}
            <span class="suggested none">{t("doctor-no-rename")}</span>
          {/if}
        </div>
      {/each}
    </div>

    <div class="actions">
      <button
        type="button"
        class="apply"
        disabled={busy || fixable.length === 0}
        onclick={applyPlan}
      >
        {t("doctor-apply", { count: fixable.length })}
      </button>
    </div>
  </section>
{/if}

<style>
  .doctor {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin: 8px 0;
    padding: 8px 10px;
    border: 1px solid var(--warn, #e4a040);
    border-radius: 6px;
    background: rgba(228, 160, 64, 0.08);
  }
  header {
    display: flex;
    align-items: baseline;
    gap: 8px;
    flex-wrap: wrap;
    font-size: 12px;
  }
  .sub {
    font-size: 11px;
    color: var(--fg-dim, #6a6a6a);
  }
  .rows {
    max-height: 180px;
    overflow: auto;
  }
  .row {
    display: grid;
    grid-template-columns: auto minmax(0, 1.4fr) minmax(0, 1fr) minmax(0, 1fr);
    align-items: center;
    gap: 8px;
    padding: 3px 0;
    font-size: 11px;
  }
  .no-fix {
    color: var(--fg-dim, #6a6a6a);
    text-align: center;
    width: 13px;
  }
  .name,
  .issues,
  .suggested {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .issues {
    color: var(--warn, #b7791f);
  }
  .suggested {
    font-family: ui-monospace, Consolas, monospace;
  }
  .suggested.none {
    font-family: inherit;
    color: var(--fg-dim, #6a6a6a);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
  }
  .apply {
    padding: 4px 10px;
    border-radius: 6px;
    font: inherit;
    font-size: 12px;
    cursor: pointer;
    background: var(--warn, #e4a040);
    border: 1px solid transparent;
    color: #1f1f1f;
    font-weight: 600;
  }
  .apply:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
</style>
