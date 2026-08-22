<!--
  Phase 37 — Settings → Mobile.
  Pair toggle + PeerJS broker URL + persistent peer-id + "Start
  pairing" button that mints a QR carrying `cthat-pair://<peer-id>?sas=…`
  for the phone PWA to scan. The PWA then connects to the desktop's
  PeerJS peer-id, derives the matching SAS emojis, the user confirms
  the four glyphs match, and the PWA commits the pairing back to the
  desktop via `mobile_pair_commit`.

  The data channel itself runs over WebRTC's DTLS — confidentiality
  + integrity are at the transport layer; no axum / TLS bind on the
  desktop. The Tauri webview hosts the PeerJS client.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onDestroy, onMount } from "svelte";

  import { pushToast } from "../stores";
  import { t } from "../i18n";
  import type { SettingsDto } from "../types";

  type MobilePairStatus = {
    serverActive: boolean;
    desktopPeerId: string;
    qrUrl: string | null;
    qrPngBase64: string | null;
  };

  let { settings = $bindable() }: { settings: SettingsDto | null } = $props();

  /** The install QR — the one `pair-no-device-warning` tells the reader to
   *  scan. It is a different code from the pairing QR below: this one
   *  carries the URL for getting the mobile app onto the phone, and the
   *  pairing QR carries `cthat-pair://<peer-id>?sas=…` and only exists
   *  once the pair-server is running. The warning promised this one and
   *  the pane never rendered it, so there was no QR to scan at the point
   *  the instructions said to scan one. */
  type MobileOnboardingDto = { url: string; qrPngBase64: string };

  let status = $state<MobilePairStatus | null>(null);
  let installQr = $state<MobileOnboardingDto | null>(null);
  let busy = $state(false);
  let pollHandle: ReturnType<typeof setInterval> | null = null;

  onMount(() => {
    void refreshStatus();
    void loadInstallQr();
  });

  async function loadInstallQr() {
    try {
      installQr = (await invoke("mobile_onboarding_qr", {
        pwaUrl: null,
      })) as MobileOnboardingDto;
    } catch (e) {
      // Non-fatal: the rest of the pane still pairs a phone that already
      // has the app.
      console.error("mobile_onboarding_qr", e);
    }
  }

  onDestroy(() => {
    stopPolling();
  });

  async function refreshStatus() {
    try {
      status = (await invoke("mobile_pair_status")) as MobilePairStatus;
      if (status.serverActive) {
        ensurePolling();
      } else {
        stopPolling();
      }
    } catch (e) {
      console.error("mobile_pair_status", e);
    }
  }

  function ensurePolling() {
    if (pollHandle != null) return;
    pollHandle = setInterval(() => {
      void refreshStatus();
    }, 2000);
  }

  function stopPolling() {
    if (pollHandle != null) {
      clearInterval(pollHandle);
      pollHandle = null;
    }
  }

  async function startPairing() {
    if (busy) return;
    busy = true;
    try {
      // The desktop now mints its own X25519 keypair server-side
      // inside `mobile_pair_start` (Phase 38 follow-up). The IPC
      // takes no arguments; the public key is included in the
      // pairing-token URL the phone scans.
      status = (await invoke("mobile_pair_start")) as MobilePairStatus;
      ensurePolling();
    } catch (e) {
      pushToast("error", `${e}`);
    } finally {
      busy = false;
    }
  }

  async function stopPairing() {
    if (busy) return;
    busy = true;
    try {
      await invoke("mobile_pair_stop");
      status = null;
      stopPolling();
      await refreshStatus();
    } catch (e) {
      pushToast("error", `${e}`);
    } finally {
      busy = false;
    }
  }

  async function revokePairing(pubkeyHex: string) {
    if (busy) return;
    busy = true;
    try {
      await invoke("mobile_revoke", { pubkeyHex });
      if (settings && settings.mobile) {
        settings = {
          ...settings,
          mobile: {
            ...settings.mobile,
            pairings: settings.mobile.pairings.filter(
              (p) => p.phonePublicKeyHex !== pubkeyHex,
            ),
          },
        };
      }
    } catch (e) {
      pushToast("error", `${e}`);
    } finally {
      busy = false;
    }
  }

  async function sendTestPush(pubkeyHex: string) {
    if (busy) return;
    busy = true;
    try {
      const message = (await invoke("mobile_send_test_push", { pubkeyHex })) as string;
      pushToast("success", t("push-toast-sent", { device: message }));
    } catch (e) {
      pushToast(
        "error",
        t("push-toast-failed", {
          device: pubkeyHex.slice(0, 8),
          reason: `${e}`,
        }),
      );
    } finally {
      busy = false;
    }
  }
</script>

<div class="mobile-panel">
  <h4 class="subheading">{t("settings-mobile-heading")}</h4>
  <p class="hint">{t("settings-mobile-hint")}</p>

  {#if settings && settings.mobile}
    <label class="row check">
      <input
        type="checkbox"
        bind:checked={settings.mobile.pairEnabled}
      />
      <span class="label">{t("settings-mobile-pair-toggle")}</span>
    </label>

    <label class="row check">
      <input
        type="checkbox"
        bind:checked={settings.mobile.autoConnect}
      />
      <span class="label">{t("pair-always-connect")}</span>
    </label>

    {#if settings.mobile.autoConnect && settings.mobile.pairings.length === 0}
      <p class="hint warn">{t("pair-no-device-warning")}</p>
    {/if}

    <!-- Gated on `pairEnabled`, not on the warning above: with only
         "Allow new pairings" ticked the warning does not show, but the
         phone still needs the app, so the QR still has to be here.

         Hidden once the pair-server is up. From that point the pairing
         QR renders below, and two unlabelled 200px codes on one screen
         is worse than none — this is the "get the app" step, that one is
         the "connect this phone" step, and they are not interchangeable. -->
    {#if settings.mobile.pairEnabled && settings.mobile.pairings.length === 0 && !status?.serverActive}
      {#if installQr}
        <img
          src={`data:image/png;base64,${installQr.qrPngBase64}`}
          alt="QR code linking to the mobile companion install page"
          class="qr"
        />
        <p class="addr">{installQr.url}</p>
      {:else}
        <p class="hint">{t("pair-onboarding-loading-qr")}</p>
      {/if}
    {/if}

    <p class="addr">
      {t("pair-desktop-peer-id")} {settings.mobile.desktopPeerId || "(minted on first pair)"}
    </p>

    <label class="row">
      <span class="label">{t("pair-broker-url")}</span>
      <input
        type="text"
        bind:value={settings.mobile.peerjsBroker}
        placeholder="0.peerjs.com"
      />
    </label>
  {/if}

  <div class="actions">
    {#if status?.serverActive}
      <button
        type="button"
        class="secondary"
        onclick={stopPairing}
        disabled={busy}
      >
        {t("settings-mobile-pair-button")} (stop)
      </button>
    {:else}
      <button
        type="button"
        class="primary"
        onclick={startPairing}
        disabled={busy || !settings?.mobile?.pairEnabled}
      >
        {t("settings-mobile-pair-button")}
      </button>
    {/if}
  </div>

  {#if status?.serverActive}
    <div class="pair-active">
      <p class="hint">{t("settings-mobile-pair-active")}</p>
      {#if status.qrPngBase64}
        <img
          src={`data:image/png;base64,${status.qrPngBase64}`}
          alt="QR pairing code"
          class="qr"
        />
      {/if}
      {#if status.qrUrl}
        <p class="addr">{status.qrUrl}</p>
      {/if}
    </div>
  {/if}

  <h4 class="subheading">{t("pair-devices-heading")}</h4>
  {#if !settings || !settings.mobile || settings.mobile.pairings.length === 0}
    <p class="hint">{t("settings-mobile-no-pairings")}</p>
  {:else}
    <ul class="pairings">
      {#each settings.mobile.pairings as entry (entry.phonePublicKeyHex)}
        <li>
          <span class="label">{entry.label}</span>
          <span class="fp">{entry.phonePublicKeyHex.slice(0, 12)}…</span>
          <button
            type="button"
            class="secondary"
            onclick={() => sendTestPush(entry.phonePublicKeyHex)}
            disabled={busy || !entry.pushTarget}
          >
            Test push
          </button>
          <button
            type="button"
            class="secondary destructive"
            onclick={() => revokePairing(entry.phonePublicKeyHex)}
            disabled={busy}
          >
            {t("settings-mobile-revoke-button")}
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .mobile-panel {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  .qr {
    width: 200px;
    height: 200px;
    margin-top: 0.5rem;
    image-rendering: pixelated;
    border: 1px solid var(--color-border, #888);
  }
  .pair-active {
    border: 1px dashed var(--color-border, #888);
    padding: 0.75rem;
    border-radius: 6px;
  }
  .pairings {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .pairings li {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.25rem 0.5rem;
    border-bottom: 1px solid var(--color-border, #444);
  }
  .pairings .fp {
    font-family: var(--font-mono, monospace);
    font-size: 0.85em;
    opacity: 0.7;
    flex: 1;
  }
  .actions {
    display: flex;
    gap: 0.5rem;
    margin-top: 0.5rem;
  }
  .addr {
    font-family: var(--font-mono, monospace);
    font-size: 0.85em;
    opacity: 0.7;
    margin: 0.25rem 0;
  }
  .hint.warn {
    background: rgba(255, 165, 0, 0.12);
    border-left: 3px solid orange;
    padding: 0.5rem 0.75rem;
    margin: 0.5rem 0;
    border-radius: 0 4px 4px 0;
  }
</style>
