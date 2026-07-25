<!--
  FFM-M14 — export the dry-run plan before the job runs.

  One button, shared by every pre-copy gate: pick a destination file,
  and the plan (per-category rows, byte totals, conflicts, and the
  FFM-M12 filename findings) is written as HTML / CSV / JSON. Reads
  only — the report file is the sole thing created.
-->
<script lang="ts">
  import { save } from "@tauri-apps/plugin-dialog";

  import Icon from "../icons/Icon.svelte";
  import { t } from "../i18n";
  import { pushToast } from "../stores";
  import { exportPreflightReport, type DryRunOptionsDto } from "../ipc";

  interface Props {
    src: string;
    dst: string;
    opts?: DryRunOptionsDto;
  }

  let {
    src,
    dst,
    opts = { forceOverwrite: false, trustSizeMtime: true },
  }: Props = $props();

  let busy = $state(false);

  async function exportReport() {
    if (busy || !src || !dst) return;
    try {
      const dest = await save({
        defaultPath: "freally-preflight.html",
        filters: [
          { name: "HTML", extensions: ["html"] },
          { name: "CSV", extensions: ["csv"] },
          { name: "JSON", extensions: ["json"] },
        ],
      });
      if (!dest || typeof dest !== "string") return;
      const ext = dest.slice(dest.lastIndexOf(".") + 1).toLowerCase();
      const format = ext === "csv" || ext === "json" ? ext : "html";
      busy = true;
      const report = await exportPreflightReport(src, dst, opts, "auto", format, dest);
      pushToast(
        "success",
        t("preflight-report-toast", {
          files: report.totalFiles,
          findings: report.filenameFindings,
        }),
      );
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    } finally {
      busy = false;
    }
  }
</script>

<button
  type="button"
  class="preflight-report"
  onclick={exportReport}
  disabled={busy || !src || !dst}
  aria-label={t("preflight-report-action")}
  title={t("preflight-report-hint")}
>
  <Icon name="download" size={14} />
  {t("preflight-report-action")}
</button>

<style>
  .preflight-report {
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
  .preflight-report:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
</style>
