<!--
  FFM-M01 — the paste-time "Copy & Paste" chooser (TeraCopy-style).

  Appears whenever files arrive via the paste hotkey, the intercepted
  OS copy verb, or a destination-less CLI enqueue. The user picks the
  engine:

  - System copy/move  — plain, fast, unverified filesystem transfer
                        (the OS-paste contrast to Freally's engine).
  - Freally           — the verified byte-exact engine via start_copy /
                        start_move (default collision handling).
  - Replace older     — same engine with `overwrite-if-newer`.
  - More options…     — hands the paths to the full DropStagingDialog
                        (sort, subset, preflight).

  A destination is picked with the native folder dialog on the first
  row click and reused for subsequent clicks while the dialog is open.
-->
<script lang="ts">
  import { open as openDialog } from "@tauri-apps/plugin-dialog";

  import Icon from "../icons/Icon.svelte";
  import { t } from "../i18n";
  import { startCopy, startMove, systemPaste } from "../ipc";
  import { closePasteChooser, pushToast, setDropped } from "../stores";
  import type { ShellEnqueueDto } from "../types";

  interface Props {
    request: ShellEnqueueDto;
  }

  let { request }: Props = $props();

  let destination = $state<string | null>(null);
  let busy = $state(false);

  const isMove = $derived(request.verb === "move");
  const sourceDir = $derived.by(() => {
    const first = request.paths[0] ?? "";
    const cut = Math.max(first.lastIndexOf("\\"), first.lastIndexOf("/"));
    return cut > 0 ? first.slice(0, cut) : first;
  });

  async function pickDestination(): Promise<string | null> {
    if (destination) return destination;
    const picked = await openDialog({ directory: true, multiple: false });
    if (typeof picked === "string" && picked.length > 0) {
      destination = picked;
      return picked;
    }
    return null;
  }

  type Engine = "system" | "freally" | "replace-older";

  async function run(engine: Engine) {
    if (busy) return;
    busy = true;
    try {
      const dest = await pickDestination();
      if (!dest) return;
      if (engine === "system") {
        const report = await systemPaste(request.verb, request.paths, dest);
        pushToast("success", t("toast-system-paste-done", { items: report.items }));
      } else if (engine === "freally") {
        if (isMove) await startMove(request.paths, dest);
        else await startCopy(request.paths, dest);
      } else {
        const options = { collision: "overwrite-if-newer" as const };
        if (isMove) await startMove(request.paths, dest, options);
        else await startCopy(request.paths, dest, options);
      }
      closePasteChooser();
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    } finally {
      busy = false;
    }
  }

  function moreOptions() {
    setDropped(request.paths);
    closePasteChooser();
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") closePasteChooser();
  }
</script>

<div
  class="backdrop"
  role="dialog"
  aria-modal="true"
  aria-labelledby="paste-chooser-title"
  tabindex="-1"
  onkeydown={onKeydown}
>
  <div class="panel">
    <div class="head">
      <h2 id="paste-chooser-title">{t("paste-chooser-title")}</h2>
      <button
        type="button"
        class="icon-btn"
        aria-label={t("paste-chooser-close")}
        onclick={closePasteChooser}
      >
        <Icon name="x" size={14} />
      </button>
    </div>

    <div class="route">
      <span class="arrow" aria-hidden="true">↓</span>
      <div class="paths">
        <span class="path" title={sourceDir}>{sourceDir}</span>
        <span class="path dest" title={destination ?? ""}>
          {destination ?? t("paste-chooser-files", { count: request.paths.length })}
        </span>
      </div>
    </div>

    <div class="choices" role="group" aria-label={t("paste-chooser-title")}>
      <button type="button" class="choice" disabled={busy} onclick={() => run("system")}>
        <span class="glyph"><Icon name="folder" size={18} /></span>
        <span class="text">
          <span class="title">
            {isMove ? t("paste-chooser-system-move") : t("paste-chooser-system-copy")}
          </span>
          <span class="sub">{t("paste-chooser-system-hint")}</span>
        </span>
      </button>

      <button type="button" class="choice" disabled={busy} onclick={() => run("freally")}>
        <span class="glyph brand"><Icon name="copy" size={18} /></span>
        <span class="text">
          <span class="title">
            {isMove ? t("paste-chooser-freally-move") : t("paste-chooser-freally-copy")}
          </span>
          <span class="sub">{t("paste-chooser-freally-hint")}</span>
        </span>
      </button>

      <button
        type="button"
        class="choice"
        disabled={busy}
        onclick={() => run("replace-older")}
      >
        <span class="glyph brand"><Icon name="copy" size={18} /></span>
        <span class="text">
          <span class="title">{t("paste-chooser-replace-older")}</span>
          <span class="sub">{t("paste-chooser-replace-older-hint")}</span>
        </span>
      </button>
    </div>

    <button type="button" class="more" onclick={moreOptions}>
      {t("paste-chooser-more")}
    </button>
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
    width: 100%;
    max-width: 400px;
    background: var(--surface, #ffffff);
    border: 1px solid var(--border, rgba(0, 0, 0, 0.1));
    border-radius: 8px;
    box-shadow: 0 16px 48px rgba(0, 0, 0, 0.3);
    overflow: hidden;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 10px 12px 6px;
  }
  h2 {
    margin: 0;
    font-size: 14px;
    color: var(--fg-strong, #1f1f1f);
  }
  .icon-btn {
    background: none;
    border: none;
    padding: 4px;
    cursor: pointer;
    color: var(--fg-dim, #6a6a6a);
    border-radius: 4px;
  }
  .icon-btn:hover {
    background: var(--hover, rgba(0, 0, 0, 0.04));
    color: var(--fg, #1f1f1f);
  }
  .route {
    display: flex;
    gap: 8px;
    align-items: center;
    padding: 0 12px 10px;
  }
  .arrow {
    color: var(--fg-dim, #6a6a6a);
    font-size: 14px;
  }
  .paths {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .path {
    font-size: 11px;
    color: var(--fg-dim, #6a6a6a);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .path.dest {
    color: var(--fg, #1f1f1f);
  }
  .choices {
    display: flex;
    flex-direction: column;
    border-top: 1px solid var(--border, rgba(0, 0, 0, 0.1));
  }
  .choice {
    display: flex;
    gap: 10px;
    align-items: center;
    padding: 10px 12px;
    background: none;
    border: none;
    border-bottom: 1px solid var(--border, rgba(0, 0, 0, 0.08));
    text-align: start;
    cursor: pointer;
    color: inherit;
  }
  .choice:hover:not(:disabled),
  .choice:focus-visible {
    background: var(--row-selected, rgba(79, 140, 255, 0.1));
    outline: none;
  }
  .choice:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .glyph {
    flex-shrink: 0;
    display: flex;
    color: var(--fg-dim, #6a6a6a);
  }
  .glyph.brand {
    color: var(--accent, #4f8cff);
  }
  .text {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .title {
    font-size: 13px;
    font-weight: 600;
    color: var(--fg-strong, #1f1f1f);
  }
  .sub {
    font-size: 11px;
    color: var(--fg-dim, #6a6a6a);
  }
  .more {
    background: none;
    border: none;
    padding: 8px 12px;
    font-size: 11px;
    color: var(--accent, #4f8cff);
    cursor: pointer;
    text-align: start;
  }
  .more:hover {
    text-decoration: underline;
  }
</style>
