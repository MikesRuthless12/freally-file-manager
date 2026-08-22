<!--
  Phase 37 follow-up #2 — first-launch mobile-companion onboarding
  modal.

  Shown once, on the first launch where:
    - settings.mobile?.pairEnabled is false (or unset)
    - settings.mobile.pairings is empty
    - settings.general.mobileOnboardingDismissed is not yet true

  Renders the app icon from `public/app-icons/`, the install-QR PNG (the QR scans the
  phone's camera straight to the deployed PWA URL — phone tap "Add
  to Home Screen" to install), and two actions:
    - "I have the app, pair now" → opens Settings → Mobile.
    - "Maybe later" → flips mobileOnboardingDismissed so the modal
       doesn't reappear.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { escapeToClose } from "../a11y";
  import { t } from "../i18n";
  import { onMount } from "svelte";

  type Props = {
    open: boolean;
    onClose: () => void;
    onOpenSettings: () => void;
  };

  let { open, onClose, onOpenSettings }: Props = $props();

  let qrPngBase64 = $state<string | null>(null);
  let pwaUrl = $state<string | null>(null);
  let busy = $state(false);

  onMount(async () => {
    await loadQr();
  });

  async function loadQr() {
    try {
      const dto = (await invoke("mobile_onboarding_qr", {
        pwaUrl: null,
      })) as { url: string; qrPngBase64: string };
      qrPngBase64 = dto.qrPngBase64;
      pwaUrl = dto.url;
    } catch (e) {
      console.error("mobile_onboarding_qr", e);
    }
  }

  async function dismiss() {
    if (busy) return;
    busy = true;
    try {
      await invoke("mobile_onboarding_dismiss");
    } finally {
      busy = false;
      onClose();
    }
  }

  async function openSettings() {
    if (busy) return;
    busy = true;
    try {
      await invoke("mobile_onboarding_dismiss");
    } finally {
      busy = false;
      onOpenSettings();
      onClose();
    }
  }
</script>

{#if open}
  <div
    class="backdrop"
    role="dialog"
    aria-modal="true"
    aria-labelledby="mobile-onboarding-title"
    tabindex="-1"
    use:escapeToClose={dismiss}
  >
    <div class="modal">
      <!-- `/app-icons/…`, not `/icons/…`: the latter was never a real
           path. Nothing under `public/` is called `icons`, and no file
           named `icon-128.png` exists anywhere in the repo, so this
           rendered as a broken-image glyph with its alt text beside it
           every time the modal opened. The app icons that DO reach the
           webview live in `public/app-icons/`; `src-tauri/icons/` is the
           bundler's input and is never served. -->
      <img
        src="/app-icons/freally-file-manager.png"
        alt=""
        class="logo"
      />
      <h2 id="mobile-onboarding-title">{t("pair-onboarding-title")}</h2>
      <p class="hint">{t("pair-onboarding-body")}</p>
      {#if qrPngBase64}
        <img
          class="qr"
          src={`data:image/png;base64,${qrPngBase64}`}
          alt="Install QR"
        />
        <p class="addr">{pwaUrl}</p>
      {:else}
        <p class="hint muted">{t("pair-onboarding-loading-qr")}</p>
      {/if}
      <div class="actions">
        <button
          type="button"
          class="primary"
          onclick={openSettings}
          disabled={busy}
        >{t("pair-onboarding-have-app")}</button>
        <button
          type="button"
          class="secondary"
          onclick={dismiss}
          disabled={busy}
        >{t("pair-onboarding-later")}</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
    padding: 1rem;
  }
  .modal {
    background: var(--color-panel, #1e293b);
    color: var(--color-fg, #f1f5f9);
    padding: 1.25rem;
    border-radius: 0.75rem;
    max-width: 460px;
    width: 100%;
    /* Never taller than the window it sits in. Without this the modal
       grows with its content — a longer translation or a taller QR and
       it reaches the top and bottom edges again, which is what made it
       read as a full-height page instead of a centred dialog. */
    max-height: calc(100vh - 2rem);
    overflow-y: auto;
    box-sizing: border-box;
    text-align: center;
    box-shadow: 0 16px 48px rgba(0, 0, 0, 0.4);
  }
  .logo {
    width: 52px;
    height: 52px;
    margin-bottom: 0.5rem;
    border-radius: 10px;
  }
  h2 {
    margin: 0 0 0.4rem 0;
    font-size: 1.1rem;
    line-height: 1.3;
  }
  .hint {
    margin: 0.35rem 0;
    font-size: 0.875rem;
    line-height: 1.4;
    opacity: 0.85;
  }
  .qr {
    /* 150px still scans comfortably from a phone held at arm's length,
       and it is the single biggest saving in the modal's height. */
    margin: 0.7rem auto;
    width: 150px;
    height: 150px;
    image-rendering: pixelated;
    border: 3px solid var(--color-border, #475569);
    border-radius: 8px;
  }
  .addr {
    font-family: var(--font-mono, monospace);
    font-size: 0.75rem;
    opacity: 0.6;
    word-break: break-all;
    margin: 0.25rem 0;
  }
  .actions {
    display: flex;
    flex-direction: column;
    gap: 0.45rem;
    margin-top: 0.75rem;
  }
  .actions button {
    width: 100%;
    padding: 0.6rem 1rem;
    border-radius: 0.5rem;
    border: 0;
    cursor: pointer;
    font-size: 1rem;
  }
  .actions .primary {
    background: var(--color-accent, #3b82f6);
    color: white;
  }
  .actions .secondary {
    background: transparent;
    color: var(--color-fg, #f1f5f9);
    border: 1px solid var(--color-border, #475569);
  }
</style>
