<!--
  "More Freally apps" — the vendored Central panel (view-only) as a React island
  inside this Svelte dialog. Svelte owns the modal chrome + lifecycle; React owns
  the panel subtree, localized through our Fluent i18n bridge.
-->
<script lang="ts">
  import type { Root } from "react-dom/client";
  import { i18nVersion, t } from "../i18n";

  interface Props {
    open: boolean;
    onClose: () => void;
  }
  let { open, onClose }: Props = $props();

  let host = $state<HTMLDivElement | null>(null);
  let root: Root | null = null;
  // Lazily imported so React + the vendored panel are a separate async chunk,
  // loaded only when this dialog first opens.
  let island: typeof import("../../more-apps/mount") | null = null;

  async function ensureMounted() {
    if (!host || root) return;
    island ??= await import("../../more-apps/mount");
    if (host && !root) root = island.mountMoreApps(host);
  }
  function teardown() {
    if (root) {
      root.unmount();
      root = null;
    }
  }

  // Mount when open (the container is bound by the time this post-DOM effect
  // runs); unmount when closed.
  $effect(() => {
    if (open && host) void ensureMounted();
    else if (!open) teardown();
  });

  // Re-render the React tree when the app language changes so the panel
  // re-localizes through the bridge.
  $effect(() => {
    void $i18nVersion;
    if (root && island) island.refreshMoreApps(root);
  });

  // Teardown if the component itself is destroyed while still open.
  $effect(() => teardown);
</script>

{#if open}
  <div
    class="backdrop"
    role="presentation"
    onclick={onClose}
    onkeydown={(e) => e.key === "Escape" && onClose()}
  >
    <div
      class="modal"
      role="dialog"
      tabindex="-1"
      aria-modal="true"
      aria-label={t("moreapps-title")}
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => e.stopPropagation()}
    >
      <header class="head">
        <h2>{t("moreapps-title")}</h2>
        <button type="button" class="close" aria-label={t("moreapps-title")} onclick={onClose}>
          ×
        </button>
      </header>
      <div class="body" bind:this={host}></div>
    </div>
  </div>
{/if}

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.45);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 96;
  }
  .modal {
    display: flex;
    flex-direction: column;
    width: min(1000px, 92vw);
    height: min(720px, 86vh);
    background: var(--surface, #ffffff);
    color: var(--fg, #1f1f1f);
    border: 1px solid var(--border, rgba(128, 128, 128, 0.3));
    border-radius: 10px;
    box-shadow: 0 12px 48px rgba(0, 0, 0, 0.4);
    overflow: hidden;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 16px;
    border-bottom: 1px solid var(--border, rgba(128, 128, 128, 0.3));
  }
  .head h2 {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
  }
  .close {
    background: transparent;
    border: 0;
    color: var(--muted, #6a6a6a);
    font-size: 22px;
    line-height: 1;
    cursor: pointer;
    padding: 0 6px;
  }
  .body {
    flex: 1 1 auto;
    min-height: 0;
    overflow: auto;
    /* Map the panel's --fcp-* tokens onto this app's theme (dark defaults ship
       with the panel, so these only refine the palette). */
    --fcp-panel: var(--surface);
    --fcp-card: var(--surface-alt, var(--surface));
    --fcp-text: var(--fg);
    --fcp-muted: var(--muted);
    --fcp-accent: var(--accent);
    --fcp-accent-1: var(--accent);
    --fcp-accent-2: var(--accent);
    --fcp-border: var(--border);
  }
</style>
