<!--
  Phase 12 — full Settings modal. Six tabs mirroring the Phase 12
  build-prompt spec:

  - General       : language, theme, start-with-OS, single-instance, minimize-to-tray
  - Transfer      : buffer size, verify, concurrency, reflink, fsync, preserve *
  - Shell         : context menu, intercept default copy (Win), notify on completion
  - Secure delete : method, confirm twice
  - Advanced      : log level, telemetry (always off, displayed as read-only),
                    error policy, history retention, database path
  - Profiles      : save / load / delete / export / import named configs

  Wire-shape: a single `SettingsDto` flows both ways through IPC.
  `get_settings` loads on open; every control change invokes
  `update_settings` so the Rust side both persists to TOML and
  republishes to the live `AppState.settings` lock — the engine then
  picks up new values on the next enqueue without a restart.

  Phase 11b shipped the skeleton (one General tab); Phase 12 fills
  in the remaining five + profile management. Language switcher
  semantics (Intl.DisplayNames rendering + English-pinned-first
  ordering) carried over from Phase 11b.
-->
<script lang="ts">
  import { save as saveDialog, open as openDialog } from "@tauri-apps/plugin-dialog";
  import { escapeToClose } from "../a11y";
  import { isOpenableUrl, parseReleaseNotes } from "../release-notes";
  import { invoke } from "@tauri-apps/api/core";

  /// Hand a release-notes link to the OS browser.
  ///
  /// Never navigates the webview: this window IS the app, so following
  /// a link in place would replace it with a web page and strand the
  /// user. Re-checks the scheme even though `parseReleaseNotes` only
  /// emits http(s) — the notes are remote text, so the click path is
  /// guarded on its own terms rather than by trusting its caller.
  async function openReleaseLink(href: string) {
    if (!isOpenableUrl(href)) return;
    try {
      const opener = await import("@tauri-apps/plugin-opener");
      await opener.openUrl(href);
    } catch {
      // No opener (browser harness, or the plugin is unavailable) —
      // the URL is still on screen to copy.
    }
  }

  import Icon from "../icons/Icon.svelte";
  import MobilePanel from "./MobilePanel.svelte";
  import ProvenanceTab from "./ProvenanceTab.svelte";
  import RemotesTab from "./RemotesTab.svelte";
  import SanitizeTab from "./SanitizeTab.svelte";
  import PluginsTab from "./PluginsTab.svelte";
  // Build 3 — FFM-M17 / M18 / M20 each get their own tab; they are
  // list editors, not single toggles, so folding them into an existing
  // pane would bury them.
  import SchedulerPanel from "./SchedulerPanel.svelte";
  import QueueAffinityPanel from "./QueueAffinityPanel.svelte";
  import FavoritesPanel from "./FavoritesPanel.svelte";
  import { errorText } from "../errors";
  import {
    collabRoster,
    collabAddMember,
    collabRemoveMember,
    collabGenerateIdentity,
    collabSas,
    mergeFfmpegStatus,
    mergeFfmpegPrefsGet,
    mergeFfmpegPrefsSet,
    type FfmpegStatusDto,
    type CollabRosterDto,
  } from "../ipc";
  import {
    bugReportContext,
    bugReportPreview,
    bugReportSubmit,
    bugReportClearCrash,
    bugReportSimulate,
    type BugReportContextDto,
  } from "../ipc";
  import { i18nVersion, locale, setLocale, t } from "../i18n";
  import {
    closeSettings,
    pinnedDestinations,
    pushToast,
    refreshPinnedDestinations,
    settingsOpen,
    setErrorDisplayMode,
  } from "../stores";
  import {
    deleteProfile,
    exportProfile,
    getSettings,
    importProfile,
    listProfiles,
    loadProfile,
    queuePinDestination,
    queueUnpinDestination,
    resetSettings,
    saveProfile,
    serverStart,
    serverStatus,
    serverStop,
    shellCopyInterceptStatus,
    shellRevertOsCopyHandler,
    updateSettings,
    updaterCheckNow,
    updaterDismissVersion,
    // Build 3 — FFM-M21 portable status, FFM-M24 launch at login.
    autostartSet,
    autostartStatus,
    portableStatus,
  } from "../ipc";
  import type { AutostartStatusDto, PortableStatusDto } from "../types";
  import type { CopyInterceptStatus } from "../ipc";
  import type { ServerStatusDto } from "../ipc";
  import type {
    PinnedDestinationDto,
    ProfileInfoDto,
    SettingsDto,
    UpdateCheckDto,
    WebhookDto,
  } from "../types";

  type TabId =
    | "general"
    | "transfer"
    | "filters"
    | "shell"
    | "secure-delete"
    | "advanced"
    | "updater"
    | "network"
    | "power"
    | "remotes"
    | "mobile"
    | "provenance"
    | "bugreport"
    | "collab"
    | "sanitize"
    | "plugins"
    | "server"
    | "schedules"
    | "queues"
    | "favorites"
    | "profiles";

  let activeTab: TabId = $state("general");
  let settings = $state<SettingsDto | null>(null);
  let profiles = $state<ProfileInfoDto[]>([]);
  let profileNameInput = $state("");
  let busy = $state(false);

  // FFM-M21 / FFM-M24 — both read live OS state, not the persisted
  // preference, so the pane tells the truth after a login item was
  // removed by hand or the app was launched from a portable stick.
  let portable = $state<PortableStatusDto | null>(null);
  let autostart = $state<AutostartStatusDto | null>(null);
  let autostartError = $state("");

  async function loadSystemStatus() {
    try {
      // Normalise to `null`: every reader below guards on `!== null`,
      // and a probe that resolves with nothing would otherwise sail
      // past that guard and crash the pane on `.supported`.
      const [p, a] = await Promise.all([portableStatus(), autostartStatus()]);
      portable = p ?? null;
      autostart = a ?? null;
    } catch (e) {
      autostartError = errorText(e);
    }
  }

  async function onAutostartToggle(event: Event) {
    const wanted = (event.currentTarget as HTMLInputElement).checked;
    autostartError = "";
    try {
      autostart = await autostartSet(wanted);
      if (settings !== null) settings.general.startWithOs = autostart.enabled;
    } catch (e) {
      autostartError = errorText(e);
      // Re-read so the checkbox snaps back to what the OS actually
      // holds rather than showing a state we failed to reach.
      autostart = (await autostartStatus()) ?? null;
    }
  }

  // Native self-name (endonym) for each shipped locale — shown verbatim in
  // the picker so a user finds their language regardless of the active UI
  // language. Each value is the language's own name.
  const ENDONYMS: Record<string, string> = {
    en: "English",
    es: "Español",
    "zh-CN": "简体中文",
    hi: "हिन्दी",
    ar: "العربية",
    "pt-BR": "Português (Brasil)",
    ru: "Русский",
    ja: "日本語",
    de: "Deutsch",
    fr: "Français",
    ko: "한국어",
    it: "Italiano",
    tr: "Türkçe",
    vi: "Tiếng Việt",
    pl: "Polski",
    nl: "Nederlands",
    id: "Bahasa Indonesia",
    uk: "Українська",
  };

  // Always show the language's native self-name (endonym), never a name
  // localized to the active UI language — so the list reads the same no
  // matter which language is currently selected.
  function displayName(code: string): string {
    return ENDONYMS[code] ?? code;
  }

  // Phase 14a — HTML `<input type="date">` yields "yyyy-mm-dd" in the
  // user's local calendar; we store and compare against mtime in UTC
  // seconds so "everything before 2026-01-01" means midnight UTC
  // regardless of where the user lives. `null` on either side of the
  // boundary means "unbounded on that end".
  function secsToDateInput(secs: number | null): string {
    if (secs === null) return "";
    const d = new Date(secs * 1000);
    const y = d.getUTCFullYear().toString().padStart(4, "0");
    const m = (d.getUTCMonth() + 1).toString().padStart(2, "0");
    const day = d.getUTCDate().toString().padStart(2, "0");
    return `${y}-${m}-${day}`;
  }

  function dateInputToSecs(input: string): number | null {
    if (!input) return null;
    const parts = input.split("-");
    if (parts.length !== 3) return null;
    const y = parseInt(parts[0], 10);
    const m = parseInt(parts[1], 10);
    const d = parseInt(parts[2], 10);
    if (!Number.isFinite(y) || !Number.isFinite(m) || !Number.isFinite(d)) return null;
    return Math.floor(Date.UTC(y, m - 1, d) / 1000);
  }

  // Fixed display order: English first, then the rest alphabetically by their
  // English language name. Independent of the active locale, so the picker
  // never reorders when the user switches languages.
  const LOCALE_ORDER = [
    "en", "ar", "zh-CN", "nl", "fr", "de", "hi", "id", "it",
    "ja", "ko", "pl", "pt-BR", "ru", "es", "tr", "uk", "vi",
  ];
  const orderedLocales = $derived.by<string[]>(() => {
    const all = $locale.available;
    const known = LOCALE_ORDER.filter((c) => all.includes(c));
    const extra = all.filter((c) => !LOCALE_ORDER.includes(c));
    return [...known, ...extra];
  });

  // Load settings + profiles whenever the modal opens. Skip if we
  // already have them — a modal close/re-open shouldn't cost a
  // full IPC round-trip unless the user explicitly reset.
  $effect(() => {
    if ($settingsOpen && settings === null) {
      void refresh();
    }
  });

  async function refresh() {
    busy = true;
    try {
      // FFM-M21 / FFM-M24 — the two status probes are independent of
      // the settings read and of each other, so all four go out at
      // once. The General pane then paints the portable banner and the
      // real autostart state on first render rather than flashing a
      // wrong checkbox.
      const [s, p] = await Promise.all([
        getSettings(),
        listProfiles(),
        loadSystemStatus(),
      ]);
      settings = s;
      profiles = p;
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    } finally {
      busy = false;
    }
  }

  /// Commit the current in-memory `settings` back to Rust. Called
  /// after every control change (the controls write through `bind:`
  /// to the `settings` state and then we push). We don't debounce
  /// at this layer — the Rust `update_settings` handler is cheap and
  /// TOML writes are atomic, so an 80 ms slider scrub triggers maybe
  /// a dozen writes worst case.
  async function pushSettings() {
    if (!settings) return;
    try {
      const next = await updateSettings(settings);
      settings = next;
      // Mirror persisted UI-render preferences into the live store
      // so components outside this modal (ErrorModal / ErrorPromptDrawer
      // in App.svelte) re-render against the new value without a
      // follow-up IPC round-trip.
      setErrorDisplayMode(next.general.errorDisplayMode);
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  async function onLocaleChange(e: Event) {
    const target = e.currentTarget as HTMLSelectElement;
    const code = target.value;
    if (!settings) return;
    settings = { ...settings, general: { ...settings.general, language: code } };
    // Hot-swap the webview BEFORE the server push so the rest of the
    // modal re-renders in the new language immediately.
    await setLocale(code);
    await pushSettings();
  }

  // FFM-M01 — live copy-interceptor state for the Shell tab. Loaded
  // when the tab opens and re-probed after any shell-toggle push, so
  // the UI reflects reality (e.g. the toggle is on but the handler
  // isn't registered, so interception never actually armed).
  let interceptStatus = $state<CopyInterceptStatus | null>(null);

  async function loadInterceptStatus() {
    try {
      interceptStatus = await shellCopyInterceptStatus();
    } catch {
      interceptStatus = null;
    }
  }

  $effect(() => {
    if (activeTab === "shell" && interceptStatus === null) {
      void loadInterceptStatus();
    }
  });

  // Shell toggles route through here so the status line re-probes.
  async function pushShellSettings() {
    await pushSettings();
    await loadInterceptStatus();
  }

  async function onRevertCopyHandler() {
    try {
      await shellRevertOsCopyHandler();
      settings = await getSettings();
      await loadInterceptStatus();
      pushToast("success", "toast-copy-handler-reverted");
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  async function onResetAll() {
    if (!confirm(t("settings-reset-confirm"))) return;
    try {
      const next = await resetSettings();
      settings = next;
      setErrorDisplayMode(next.general.errorDisplayMode);
      pushToast("success", "toast-settings-reset");
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  // ---- Profiles ------------------------------------------------------

  async function onSaveProfile() {
    const name = profileNameInput.trim();
    if (!name) return;
    try {
      await saveProfile(name);
      profileNameInput = "";
      profiles = await listProfiles();
      pushToast("success", "toast-profile-saved");
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  async function onLoadProfile(name: string) {
    try {
      const next = await loadProfile(name);
      settings = next;
      setErrorDisplayMode(next.general.errorDisplayMode);
      pushToast("info", "toast-profile-loaded");
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  async function onDeleteProfile(name: string) {
    try {
      await deleteProfile(name);
      profiles = await listProfiles();
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  async function onExportProfile(name: string) {
    const dest = await saveDialog({
      defaultPath: `${name}.json`,
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (typeof dest !== "string") return;
    try {
      await exportProfile(name, dest);
      pushToast("success", "toast-profile-exported");
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  async function onImportProfile() {
    const src = await openDialog({
      multiple: false,
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (typeof src !== "string") return;
    const name = prompt(t("settings-profile-import-prompt"));
    if (!name) return;
    try {
      await importProfile(name, src);
      profiles = await listProfiles();
      pushToast("success", "toast-profile-imported");
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  // ---- Error-policy helper ------------------------------------------
  // The ErrorPolicy enum's three inline-args shape doesn't bind cleanly
  // with `<select>`, so we carry the variant choice in a plain string
  // and write back a structured object on change.
  function errorPolicyKind(
    ep: SettingsDto["advanced"]["errorPolicy"],
  ): string {
    return ep.kind;
  }

  function setErrorPolicy(kind: string) {
    if (!settings) return;
    let next: SettingsDto["advanced"]["errorPolicy"];
    switch (kind) {
      case "skip":
        next = { kind: "skip" };
        break;
      case "abort":
        next = { kind: "abort" };
        break;
      case "retryN":
        next = { kind: "retryN", maxAttempts: 3, backoffMs: 250 };
        break;
      default:
        next = { kind: "ask" };
    }
    settings = {
      ...settings,
      advanced: { ...settings.advanced, errorPolicy: next },
    };
    void pushSettings();
  }

  // ---- Phase 45.6 — tray-pinned destinations ----------------------
  // Local form state for the "Add destination" row. Persisted state
  // lives in `pinnedDestinations` (Rust → settings TOML); this panel
  // round-trips through `queuePinDestination` / `queueUnpinDestination`.
  let trayPinLabel = $state("");
  let trayPinPath = $state("");

  async function onPickTrayPinPath() {
    try {
      const picked = await openDialog({ multiple: false, directory: true });
      if (typeof picked === "string" && picked.length > 0) {
        trayPinPath = picked;
      }
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  async function onAddTrayPin() {
    const label = trayPinLabel.trim();
    const path = trayPinPath.trim();
    // Silent no-op on empty inputs — the Add button is visible but
    // the user sees the row simply not appear, matching the
    // "Settings panel is responsive, never shouts" pattern of the
    // rest of this modal.
    if (label.length === 0 || path.length === 0) return;
    try {
      await queuePinDestination(label, path);
      await refreshPinnedDestinations();
      trayPinLabel = "";
      trayPinPath = "";
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  async function onRemoveTrayPin(row: PinnedDestinationDto) {
    try {
      await queueUnpinDestination(row.label, row.path);
      await refreshPinnedDestinations();
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  // ---- Phase 15 updater -----------------------------------------------
  // Most-recent check result; populated on demand. Rendered below the
  // channel selector when non-null.
  let lastCheck = $state<UpdateCheckDto | null>(null);
  let checking = $state(false);

  async function onCheckForUpdatesNow() {
    checking = true;
    try {
      // `force: true` bypasses the 24 h throttle — the UI button is
      // an explicit user action so the throttle doesn't apply.
      const res = await updaterCheckNow(true, null);
      lastCheck = res;
      // The backend bumped `lastCheckUnixSecs` on its side; re-pull
      // settings so the displayed timestamp stays in sync without the
      // user having to close+reopen the modal.
      if (settings) {
        const s = await getSettings();
        settings = s;
      }
      if (res.isNewer) {
        pushToast("info", "toast-update-available");
      } else if (res.availableVersion) {
        pushToast("success", "toast-update-up-to-date");
      }
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    } finally {
      checking = false;
    }
  }

  /**
   * Phase 34 — Settings → Advanced → Audit log actions.
   * `onAuditTestWrite` emits a synthetic LoginEvent through the live
   * sink so the user can confirm the log file is reachable; the
   * backend refuses when audit is disabled or the sink failed to
   * open. `onAuditVerifyChain` recomputes the BLAKE3 chain hash end-
   * to-end and surfaces the summary as a toast.
   */
  async function onAuditTestWrite() {
    try {
      await invoke("audit_test_write");
      pushToast("success", "toast-audit-test-write-ok");
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  async function onAuditVerifyChain() {
    try {
      const report = (await invoke("audit_verify")) as {
        total: number;
        matches: number;
        mismatches: number;
        missing: number;
      };
      if (report.mismatches === 0 && report.missing === 0) {
        pushToast("success", "toast-audit-verify-ok");
      } else {
        pushToast("error", "toast-audit-verify-failed");
      }
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  async function onDismissAvailable() {
    if (!lastCheck || !lastCheck.availableVersion) return;
    try {
      await updaterDismissVersion(lastCheck.availableVersion);
      if (settings) {
        settings = {
          ...settings,
          updater: {
            ...settings.updater,
            dismissedVersion: lastCheck.availableVersion,
          },
        };
      }
      lastCheck = null;
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  function formatLastCheckLabel(unixSecs: number): string {
    if (!unixSecs || unixSecs <= 0) return t("settings-updater-last-never");
    const d = new Date(unixSecs * 1000);
    return d.toLocaleString();
  }

  // ---- Phase 48 — Settings → Server ---------------------------------
  // The form binds to `settings.server.*` and persists through the
  // standard `pushSettings()` path; the Start / Stop control drives the
  // live `ServerHandle` via the dedicated `server_*` IPC commands. The
  // status line + metrics URL come from `serverStatus`.
  let serverState = $state<ServerStatusDto | null>(null);
  let serverBusy = $state(false);

  // Pull the live status the first time the Server tab is opened.
  $effect(() => {
    if (activeTab === "server" && serverState === null) {
      void refreshServerStatus();
    }
  });

  async function refreshServerStatus() {
    try {
      serverState = await serverStatus();
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  async function onServerStart() {
    serverBusy = true;
    try {
      // Persist the form first so `server_start` builds its config from
      // the values the user just edited.
      await pushSettings();
      serverState = await serverStart();
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    } finally {
      serverBusy = false;
    }
  }

  async function onServerStop() {
    serverBusy = true;
    try {
      serverState = await serverStop();
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    } finally {
      serverBusy = false;
    }
  }

  function onAddWebhook() {
    if (!settings) return;
    const next: WebhookDto = {
      target: "slack",
      url: "",
      pushoverToken: "",
      pushoverUser: "",
    };
    settings = {
      ...settings,
      server: {
        ...settings.server,
        webhooks: [...settings.server.webhooks, next],
      },
    };
    void pushSettings();
  }

  function onRemoveWebhook(index: number) {
    if (!settings) return;
    settings = {
      ...settings,
      server: {
        ...settings.server,
        webhooks: settings.server.webhooks.filter((_, i) => i !== index),
      },
    };
    void pushSettings();
  }

  async function onPickServerRoot() {
    if (!settings) return;
    try {
      const picked = await openDialog({ multiple: false, directory: true });
      if (typeof picked === "string" && picked.length > 0) {
        settings = {
          ...settings,
          server: { ...settings.server, root: picked },
        };
        await pushSettings();
      }
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }
  // Phase 53 — decoded video thumbnails are opt-in and run against an
  // ffmpeg the user installed themselves. Nothing is bundled, which is
  // exactly what keeps ffmpeg's LGPL terms off this project.
  let ffmpeg = $state<FfmpegStatusDto | null>(null);
  let ffmpegEnabled = $state(false);
  let ffmpegPath = $state("");

  $effect(() => {
    if (activeTab !== "collab" || ffmpeg !== null) return;
    void (async () => {
      try {
        ffmpeg = await mergeFfmpegStatus();
        const prefs = await mergeFfmpegPrefsGet();
        ffmpegEnabled = prefs.enabled;
        ffmpegPath = prefs.path;
      } catch (e) {
        console.error("[merge_ffmpeg_status]", e);
      }
    })();
  });

  async function saveFfmpegPrefs(): Promise<void> {
    try {
      await mergeFfmpegPrefsSet({ enabled: ffmpegEnabled, path: ffmpegPath });
      // Re-probe: the path may now resolve, or stop resolving.
      ffmpeg = await mergeFfmpegStatus();
    } catch (e) {
      console.error("[merge_ffmpeg_prefs_set]", e);
    }
  }

  // Phase 51 — collaboration roster. Recipients are public keys, so
  // nothing shown in this list is a secret; the one secret the panel
  // ever produces is a freshly generated identity, displayed once and
  // stored nowhere.
  let collab = $state<CollabRosterDto | null>(null);
  let collabLabel = $state("");
  let collabRecipient = $state("");
  let collabNewSecret = $state<string | null>(null);
  let sasLeft = $state("");
  let sasRight = $state("");
  let sasCode = $state("");

  $effect(() => {
    if (activeTab !== "collab" || collab !== null) return;
    void (async () => {
      try {
        collab = await collabRoster();
      } catch (e) {
        console.error("[collab_roster]", e);
      }
    })();
  });

  async function collabRefresh(): Promise<void> {
    try {
      collab = await collabRoster();
    } catch (e) {
      console.error("[collab_roster]", e);
    }
  }

  async function collabAdd(): Promise<void> {
    try {
      await collabAddMember(collabLabel, collabRecipient);
      collabLabel = "";
      collabRecipient = "";
      await collabRefresh();
    } catch (e) {
      console.error("[collab_add_member]", e);
    }
  }

  async function collabRemove(label: string): Promise<void> {
    try {
      await collabRemoveMember(label);
      await collabRefresh();
    } catch (e) {
      console.error("[collab_remove_member]", e);
    }
  }

  async function collabGenerate(): Promise<void> {
    try {
      const pair = await collabGenerateIdentity();
      collabNewSecret = pair[0];
      collabRecipient = pair[1];
    } catch (e) {
      console.error("[collab_generate_identity]", e);
    }
  }

  async function collabComputeSas(): Promise<void> {
    try {
      sasCode = await collabSas(sasLeft, sasRight);
    } catch (e) {
      console.error("[collab_sas]", e);
    }
  }

  // Opt-in bug reporting. Nothing here transmits: the backend only
  // opens a pre-filled GitHub / Gmail / mail-client window, and the
  // user still presses Send. The preview is the exact submitted text.
  let bugCtx = $state<BugReportContextDto | null>(null);
  let bugDescription = $state("");
  let bugIncludeCrash = $state(true);
  let bugPreview = $state("");

  $effect(() => {
    if (activeTab !== "bugreport" || bugCtx !== null) return;
    void (async () => {
      try {
        bugCtx = await bugReportContext();
      } catch (e) {
        console.error("[bug_report_context]", e);
      }
    })();
  });

  async function refreshBugPreview(): Promise<void> {
    try {
      bugPreview = await bugReportPreview(
        bugDescription,
        bugIncludeCrash && !!bugCtx?.pendingCrash,
      );
    } catch (e) {
      console.error("[bug_report_preview]", e);
    }
  }

  async function sendBug(channel: "github" | "gmail" | "email"): Promise<void> {
    try {
      await bugReportSubmit(
        bugDescription,
        bugIncludeCrash && !!bugCtx?.pendingCrash,
        channel,
      );
    } catch (e) {
      console.error("[bug_report_submit]", e);
    }
  }

  async function dismissCrash(): Promise<void> {
    try {
      await bugReportClearCrash();
      bugCtx = await bugReportContext();
      bugPreview = "";
    } catch (e) {
      console.error("[bug_report_clear_crash]", e);
    }
  }

  async function simulateCrash(): Promise<void> {
    try {
      await bugReportSimulate();
      bugCtx = await bugReportContext();
    } catch (e) {
      console.error("[bug_report_simulate]", e);
    }
  }
</script>

{#if $settingsOpen}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <!-- Click-outside-to-dismiss. The keyboard equivalent is Escape,
       registered at the window level by `use:escapeToClose` on the
       dialog below, so it fires wherever focus happens to be — which
       an `onkeydown` on this element could not do. -->
  <div class="backdrop" role="presentation" onclick={closeSettings}>
    {#key $i18nVersion}
    <div
      class="modal"
      role="dialog"
      tabindex="-1"
      aria-modal="true"
      aria-labelledby="settings-title"
      use:escapeToClose={closeSettings}
      onclick={(e) => e.stopPropagation()}
    >
      <header>
        <h2 id="settings-title">{t("settings-title")}</h2>
        <button
          class="close"
          type="button"
          aria-label={t("action-close")}
          onclick={closeSettings}
        >
          <Icon name="x" size={16} />
        </button>
      </header>

      {#if settings === null}
        <p class="loading">{t("settings-loading")}</p>
      {:else}
        <div class="body">
          <div class="tabs" role="tablist" aria-label={t("settings-title")}>
            {#each [["general", "settings-tab-general"], ["transfer", "settings-tab-transfer"], ["filters", "settings-tab-filters"], ["shell", "settings-tab-shell"], ["secure-delete", "settings-tab-secure-delete"], ["advanced", "settings-tab-advanced"], ["updater", "settings-tab-updater"], ["network", "settings-tab-network"], ["power", "settings-tab-power"], ["remotes", "settings-tab-remotes"], ["mobile", "settings-tab-mobile"], ["provenance", "provenance-settings-heading"], ["sanitize", "sanitize-heading"], ["plugins", "settings-tab-plugins"], ["server", "settings-tab-server"], ["schedules", "settings-tab-schedules"], ["queues", "settings-tab-queues"], ["favorites", "settings-tab-favorites"], ["profiles", "settings-tab-profiles"], ["bugreport", "settings-tab-bugreport"], ["collab", "settings-tab-collab"]] as const as [id, key] (id)}
              <button
                type="button"
                role="tab"
                aria-selected={activeTab === id}
                aria-controls={`tab-${id}`}
                class:active={activeTab === id}
                onclick={() => (activeTab = id)}
              >
                {t(key)}
              </button>
            {/each}
          </div>

          <div class="tabpanel">
            {#if activeTab === "general"}
              <label class="row">
                <span class="label">{t("settings-section-language")}</span>
                <select value={$locale.code} onchange={onLocaleChange}>
                  {#each orderedLocales as code (code)}
                    <option value={code}>{displayName(code)}</option>
                  {/each}
                </select>
              </label>

              <label class="row">
                <span class="label">{t("settings-section-theme")}</span>
                <select
                  bind:value={settings.general.theme}
                  onchange={pushSettings}
                >
                  <option value="auto">{t("settings-theme-auto")}</option>
                  <option value="light">{t("settings-theme-light")}</option>
                  <option value="dark">{t("settings-theme-dark")}</option>
                </select>
              </label>

              <!-- FFM-M24 — this used to be a persisted preference with
                   nothing behind it. It now registers a real login item
                   through `autostart_set`, and reports the OS's own
                   state rather than the stored flag. -->
              <label class="row check">
                <input
                  type="checkbox"
                  checked={autostart?.enabled ?? settings.general.startWithOs}
                  disabled={autostart !== null && !autostart.supported}
                  onchange={onAutostartToggle}
                />
                <span class="label">{t("settings-autostart-label")}</span>
              </label>
              <p class="hint">{t("settings-autostart-description")}</p>
              {#if autostart !== null && !autostart.supported && autostart.reasonKey}
                <p class="hint warn" role="status">{t(autostart.reasonKey)}</p>
              {/if}
              {#if autostartError}
                <p class="hint warn" role="alert">{autostartError}</p>
              {/if}

              <!-- FFM-M21 — portable installs write beside the binary
                   and refuse the OS integrations that would outlive the
                   stick. Say so plainly instead of leaving the user to
                   wonder why toggles are disabled. -->
              {#if portable?.portable}
                <h4>{t("settings-portable-title")}</h4>
                <p class="hint">
                  {t("settings-portable-active", { path: portable.dataRoot })}
                </p>
                <p class="hint warn">
                  {t("settings-portable-keychain-warning")}
                </p>
              {/if}

              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.general.singleInstance}
                  onchange={pushSettings}
                />
                <span class="label">{t("settings-single-instance")}</span>
              </label>

              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.general.minimizeToTray}
                  onchange={pushSettings}
                />
                <span class="label">{t("settings-minimize-to-tray")}</span>
              </label>

              <label class="row">
                <span class="label">{t("settings-error-display-mode")}</span>
                <select
                  bind:value={settings.general.errorDisplayMode}
                  onchange={pushSettings}
                >
                  <option value="modal">{t("settings-error-display-modal")}</option>
                  <option value="drawer">{t("settings-error-display-drawer")}</option>
                </select>
              </label>
              <p class="hint">{t("settings-error-display-mode-hint")}</p>

              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.general.pasteShortcutEnabled}
                  onchange={pushSettings}
                />
                <span class="label">{t("settings-paste-shortcut")}</span>
              </label>
              <label class="row">
                <span class="label">{t("settings-paste-shortcut-combo")}</span>
                <input
                  type="text"
                  bind:value={settings.general.pasteShortcut}
                  onchange={pushSettings}
                  disabled={!settings.general.pasteShortcutEnabled}
                  placeholder="CmdOrCtrl+Shift+V"
                  spellcheck={false}
                />
              </label>
              <p class="hint">{t("settings-paste-shortcut-hint")}</p>

              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.general.clipboardWatcherEnabled}
                  onchange={pushSettings}
                />
                <span class="label">{t("settings-clipboard-watcher")}</span>
              </label>
              <p class="hint">{t("settings-clipboard-watcher-hint")}</p>

              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.general.autoResumeInterrupted}
                  onchange={pushSettings}
                />
                <span class="label">{t("settings-auto-resume")}</span>
              </label>
              <p class="hint">{t("settings-auto-resume-hint")}</p>

              <!-- Phase 45.6 — tray-pinned destinations. Each row
                   becomes a tray-menu item; clicking the menu item
                   stashes the destination as the active drop target
                   for the next file drop. -->
              <h4 class="subheading">{t("tray-target-section-title")}</h4>
              <p class="hint">{t("tray-target-section-hint")}</p>

              {#if $pinnedDestinations.length === 0}
                <p class="hint empty">{t("tray-target-empty")}</p>
              {:else}
                <ul class="tray-pin-list">
                  {#each $pinnedDestinations as row (row.label + "::" + row.path)}
                    <li class="tray-pin-row">
                      <span class="tray-pin-label">{row.label}</span>
                      <span class="tray-pin-path" title={row.path}>{row.path}</span>
                      <button
                        type="button"
                        class="tray-pin-remove"
                        onclick={() => onRemoveTrayPin(row)}
                        aria-label={t("tray-target-remove")}
                        title={t("tray-target-remove")}
                      >
                        ×
                      </button>
                    </li>
                  {/each}
                </ul>
              {/if}

              <div class="tray-pin-add">
                <input
                  type="text"
                  bind:value={trayPinLabel}
                  placeholder={t("tray-target-add-label")}
                  spellcheck={false}
                />
                <input
                  type="text"
                  bind:value={trayPinPath}
                  placeholder={t("tray-target-add-path")}
                  spellcheck={false}
                />
                <button
                  type="button"
                  onclick={onPickTrayPinPath}
                  class="tray-pin-pick"
                >
                  …
                </button>
                <button
                  type="button"
                  onclick={onAddTrayPin}
                  class="tray-pin-add-btn"
                >
                  {t("tray-target-add")}
                </button>
              </div>

              {#if settings.dnd}
                <h4 class="subheading">{t("settings-dnd-heading")}</h4>

                <label class="row check">
                  <input
                    type="checkbox"
                    bind:checked={settings.dnd.springLoadEnabled}
                    onchange={pushSettings}
                  />
                  <span class="label">{t("settings-dnd-spring-load")}</span>
                </label>

                <label class="row">
                  <span class="label">{t("settings-dnd-spring-delay")}</span>
                  <input
                    type="number"
                    min="200"
                    max="2000"
                    step="50"
                    bind:value={settings.dnd.springLoadDelayMs}
                    onchange={pushSettings}
                    disabled={!settings.dnd.springLoadEnabled}
                  />
                </label>

                <label class="row check">
                  <input
                    type="checkbox"
                    bind:checked={settings.dnd.showDragThumbnails}
                    onchange={pushSettings}
                  />
                  <span class="label">{t("settings-dnd-thumbnails")}</span>
                </label>

                <label class="row check">
                  <input
                    type="checkbox"
                    bind:checked={settings.dnd.highlightInvalidTargets}
                    onchange={pushSettings}
                  />
                  <span class="label">{t("settings-dnd-invalid-highlight")}</span>
                </label>
              {/if}
            {:else if activeTab === "transfer"}
              <!-- FFM-M23 — verify re-hashes the *current* source, so a
                   file rewritten mid-read verifies green. This is the
                   only control that catches it. -->
              <label class="row">
                <span class="label">
                  {t("settings-source-stability-label")}
                </span>
                <select
                  bind:value={settings.transfer.sourceStability}
                  onchange={pushSettings}
                >
                  <option value="off">
                    {t("settings-source-stability-off")}
                  </option>
                  <option value="warn">
                    {t("settings-source-stability-warn")}
                  </option>
                  <option value="recopy">
                    {t("settings-source-stability-recopy")}
                  </option>
                  <option value="fail">
                    {t("settings-source-stability-fail")}
                  </option>
                </select>
              </label>
              <p class="hint">{t("settings-source-stability-description")}</p>

              <label class="row">
                <span class="label">{t("settings-buffer-size")}</span>
                <select
                  value={String(settings.transfer.bufferSizeBytes)}
                  onchange={(e) => {
                    if (!settings) return;
                    const v = parseInt((e.currentTarget as HTMLSelectElement).value, 10);
                    settings = { ...settings, transfer: { ...settings.transfer, bufferSizeBytes: v } };
                    void pushSettings();
                  }}
                >
                  <option value="65536">64 KiB</option>
                  <option value="262144">256 KiB</option>
                  <option value="1048576">1 MiB</option>
                  <option value="4194304">4 MiB</option>
                  <option value="8388608">8 MiB</option>
                  <option value="16777216">16 MiB</option>
                </select>
              </label>

              <label class="row">
                <span class="label">{t("settings-verify")}</span>
                <select
                  bind:value={settings.transfer.verify}
                  onchange={pushSettings}
                >
                  <option value="off">{t("settings-verify-off")}</option>
                  <option value="crc32">CRC32</option>
                  <option value="md5">MD5</option>
                  <option value="sha1">SHA-1</option>
                  <option value="sha256">SHA-256</option>
                  <option value="sha512">SHA-512</option>
                  <option value="xxhash3-64">xxHash3-64</option>
                  <option value="xxhash3-128">xxHash3-128</option>
                  <option value="blake3">BLAKE3</option>
                </select>
              </label>

              <label class="row">
                <span class="label">{t("settings-concurrency")}</span>
                <select
                  bind:value={settings.transfer.concurrency}
                  onchange={pushSettings}
                >
                  <option value="auto">{t("settings-concurrency-auto")}</option>
                  {#each [1, 2, 4, 8, 12, 16] as n (n)}
                    <option value={`manual-${n}`}>{n}</option>
                  {/each}
                </select>
              </label>

              <label class="row">
                <span class="label">{t("settings-reflink")}</span>
                <select
                  bind:value={settings.transfer.reflink}
                  onchange={pushSettings}
                >
                  <option value="prefer">{t("settings-reflink-prefer")}</option>
                  <option value="avoid">{t("settings-reflink-avoid")}</option>
                  <option value="disabled">{t("settings-reflink-disabled")}</option>
                </select>
              </label>

              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.transfer.fsyncOnClose}
                  onchange={pushSettings}
                />
                <span class="label">{t("settings-fsync-on-close")}</span>
              </label>

              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.transfer.preserveTimestamps}
                  onchange={pushSettings}
                />
                <span class="label">{t("settings-preserve-timestamps")}</span>
              </label>

              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.transfer.preservePermissions}
                  onchange={pushSettings}
                />
                <span class="label">{t("settings-preserve-permissions")}</span>
              </label>

              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.transfer.preserveAcls}
                  onchange={pushSettings}
                />
                <span class="label">{t("settings-preserve-acls")}</span>
              </label>

              <label class="row check" title={t("settings-preserve-sparseness-hint")}>
                <input
                  type="checkbox"
                  bind:checked={settings.transfer.preserveSparseness}
                  onchange={pushSettings}
                />
                <span class="label">{t("settings-preserve-sparseness")}</span>
              </label>

              <label class="row check" title={t("settings-force-parallel-chunks-hint")}>
                <input
                  type="checkbox"
                  bind:checked={settings.transfer.forceParallelChunks}
                  onchange={pushSettings}
                />
                <span class="label">{t("settings-force-parallel-chunks")}</span>
              </label>

              <!-- Phase 24 — Security-metadata subsection (5 toggles) -->
              <div class="meta-subsection">
                <label
                  class="row check"
                  title={t("settings-preserve-security-metadata-hint")}
                >
                  <input
                    type="checkbox"
                    bind:checked={settings.transfer.preserveSecurityMetadata}
                    onchange={pushSettings}
                  />
                  <span class="label section-header"
                    >{t("settings-preserve-security-metadata")}</span
                  >
                </label>

                {#if settings.transfer.preserveSecurityMetadata}
                  <label
                    class="row check meta-child"
                    title={t("settings-preserve-motw-hint")}
                  >
                    <input
                      type="checkbox"
                      bind:checked={settings.transfer.preserveMotw}
                      onchange={pushSettings}
                    />
                    <span class="label">
                      {t("settings-preserve-motw")}
                      <span class="motw-warning" aria-hidden="true">⚠</span>
                    </span>
                  </label>

                  <label
                    class="row check meta-child"
                    title={t("settings-preserve-posix-acls-hint")}
                  >
                    <input
                      type="checkbox"
                      bind:checked={settings.transfer.preservePosixAcls}
                      onchange={pushSettings}
                    />
                    <span class="label">{t("settings-preserve-posix-acls")}</span>
                  </label>

                  <label
                    class="row check meta-child"
                    title={t("settings-preserve-selinux-hint")}
                  >
                    <input
                      type="checkbox"
                      bind:checked={settings.transfer.preserveSelinuxContexts}
                      onchange={pushSettings}
                    />
                    <span class="label">{t("settings-preserve-selinux")}</span>
                  </label>

                  <label
                    class="row check meta-child"
                    title={t("settings-preserve-resource-forks-hint")}
                  >
                    <input
                      type="checkbox"
                      bind:checked={settings.transfer.preserveResourceForks}
                      onchange={pushSettings}
                    />
                    <span class="label"
                      >{t("settings-preserve-resource-forks")}</span
                    >
                  </label>

                  <label class="row check meta-child">
                    <input
                      type="checkbox"
                      bind:checked={settings.transfer.appledoubleFallback}
                      onchange={pushSettings}
                    />
                    <span class="label">
                      {t("settings-appledouble-fallback")}
                    </span>
                  </label>
                {/if}
              </div>

              <label class="row">
                <span class="label">{t("settings-on-locked")}</span>
                <select
                  bind:value={settings.transfer.onLocked}
                  onchange={pushSettings}
                >
                  <option value="ask">{t("settings-on-locked-ask")}</option>
                  <option value="retry">{t("settings-on-locked-retry")}</option>
                  <option value="skip">{t("settings-on-locked-skip")}</option>
                  <option value="snapshot">{t("settings-on-locked-snapshot")}</option>
                </select>
              </label>
              <p class="hint">{t("settings-on-locked-hint")}</p>

              <h4 class="subheading">{t("settings-crypt-heading")}</h4>
              <p class="hint">{t("settings-crypt-hint")}</p>

              {#if settings.crypt}
                <label class="row">
                  <span class="label">{t("settings-crypt-encryption-mode")}</span>
                  <select
                    bind:value={settings.crypt.encryptionMode}
                    onchange={pushSettings}
                  >
                    <option value="off">{t("settings-crypt-encryption-off")}</option>
                    <option value="passphrase">{t("settings-crypt-encryption-passphrase")}</option>
                    <option value="recipients">{t("settings-crypt-encryption-recipients")}</option>
                  </select>
                </label>
                <p class="hint">{t("settings-crypt-encryption-hint")}</p>

                <label class="row">
                  <span class="label">{t("settings-crypt-recipients-file")}</span>
                  <input
                    type="text"
                    bind:value={settings.crypt.recipientsFile}
                    placeholder={t("settings-crypt-recipients-file-placeholder")}
                    disabled={settings.crypt.encryptionMode !== "recipients"}
                    onchange={pushSettings}
                  />
                </label>

                <label class="row">
                  <span class="label">{t("settings-crypt-compression-mode")}</span>
                  <select
                    bind:value={settings.crypt.compressionMode}
                    onchange={pushSettings}
                  >
                    <option value="off">{t("settings-crypt-compression-off")}</option>
                    <option value="always">{t("settings-crypt-compression-always")}</option>
                    <option value="smart">{t("settings-crypt-compression-smart")}</option>
                  </select>
                </label>
                <p class="hint">{t("settings-crypt-compression-hint")}</p>

                <label class="row">
                  <span class="label">{t("settings-crypt-compression-level")}</span>
                  <input
                    type="number"
                    min="1"
                    max="22"
                    bind:value={settings.crypt.compressionLevel}
                    disabled={settings.crypt.compressionMode === "off"}
                    onchange={pushSettings}
                  />
                </label>
                <p class="hint">{t("settings-crypt-compression-level-hint")}</p>
              {/if}
            {:else if activeTab === "filters"}
              <p class="hint">{t("settings-filters-hint")}</p>

              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.filters.enabled}
                  onchange={pushSettings}
                />
                <span class="label">{t("settings-filters-enabled")}</span>
              </label>

              <label class="row stacked">
                <span class="label">{t("settings-filters-include-globs")}</span>
                <textarea
                  rows="3"
                  placeholder={t("settings-filters-include-globs-placeholder")}
                  value={settings.filters.includeGlobs.join("\n")}
                  onchange={(e) => {
                    if (!settings) return;
                    const lines = (e.currentTarget as HTMLTextAreaElement).value
                      .split(/\r?\n/)
                      .map((s) => s.trim())
                      .filter(Boolean);
                    settings = { ...settings, filters: { ...settings.filters, includeGlobs: lines } };
                    void pushSettings();
                  }}
                ></textarea>
              </label>
              <p class="hint">{t("settings-filters-include-globs-hint")}</p>

              <label class="row stacked">
                <span class="label">{t("settings-filters-exclude-globs")}</span>
                <textarea
                  rows="3"
                  placeholder={t("settings-filters-exclude-globs-placeholder")}
                  value={settings.filters.excludeGlobs.join("\n")}
                  onchange={(e) => {
                    if (!settings) return;
                    const lines = (e.currentTarget as HTMLTextAreaElement).value
                      .split(/\r?\n/)
                      .map((s) => s.trim())
                      .filter(Boolean);
                    settings = { ...settings, filters: { ...settings.filters, excludeGlobs: lines } };
                    void pushSettings();
                  }}
                ></textarea>
              </label>
              <p class="hint">{t("settings-filters-exclude-globs-hint")}</p>

              <div class="row">
                <span class="label">{t("settings-filters-size-range")}</span>
              </div>
              <label class="row">
                <span class="label">{t("settings-filters-min-size-bytes")}</span>
                <input
                  type="number"
                  min="0"
                  value={settings.filters.minSizeBytes ?? ""}
                  onchange={(e) => {
                    if (!settings) return;
                    const v = (e.currentTarget as HTMLInputElement).value.trim();
                    const n = v === "" ? null : Math.max(0, parseInt(v, 10) || 0);
                    settings = { ...settings, filters: { ...settings.filters, minSizeBytes: n } };
                    void pushSettings();
                  }}
                />
              </label>
              <label class="row">
                <span class="label">{t("settings-filters-max-size-bytes")}</span>
                <input
                  type="number"
                  min="0"
                  value={settings.filters.maxSizeBytes ?? ""}
                  onchange={(e) => {
                    if (!settings) return;
                    const v = (e.currentTarget as HTMLInputElement).value.trim();
                    const n = v === "" ? null : Math.max(0, parseInt(v, 10) || 0);
                    settings = { ...settings, filters: { ...settings.filters, maxSizeBytes: n } };
                    void pushSettings();
                  }}
                />
              </label>

              <div class="row">
                <span class="label">{t("settings-filters-date-range")}</span>
              </div>
              <label class="row">
                <span class="label">{t("settings-filters-min-mtime")}</span>
                <input
                  type="date"
                  value={secsToDateInput(settings.filters.minMtimeUnixSecs)}
                  onchange={(e) => {
                    if (!settings) return;
                    const secs = dateInputToSecs((e.currentTarget as HTMLInputElement).value);
                    settings = { ...settings, filters: { ...settings.filters, minMtimeUnixSecs: secs } };
                    void pushSettings();
                  }}
                />
              </label>
              <label class="row">
                <span class="label">{t("settings-filters-max-mtime")}</span>
                <input
                  type="date"
                  value={secsToDateInput(settings.filters.maxMtimeUnixSecs)}
                  onchange={(e) => {
                    if (!settings) return;
                    const secs = dateInputToSecs((e.currentTarget as HTMLInputElement).value);
                    settings = { ...settings, filters: { ...settings.filters, maxMtimeUnixSecs: secs } };
                    void pushSettings();
                  }}
                />
              </label>

              <div class="row">
                <span class="label">{t("settings-filters-attributes")}</span>
              </div>
              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.filters.skipHidden}
                  onchange={pushSettings}
                />
                <span class="label">{t("settings-filters-skip-hidden")}</span>
              </label>
              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.filters.skipSystem}
                  onchange={pushSettings}
                />
                <span class="label">{t("settings-filters-skip-system")}</span>
              </label>
              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.filters.skipReadonly}
                  onchange={pushSettings}
                />
                <span class="label">{t("settings-filters-skip-readonly")}</span>
              </label>
            {:else if activeTab === "shell"}
              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.shell.contextMenuEnabled}
                  onchange={pushShellSettings}
                />
                <span class="label">{t("settings-context-menu")}</span>
              </label>
              <p class="hint">{t("settings-context-menu-hint")}</p>

              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.shell.interceptDefaultCopy}
                  onchange={pushShellSettings}
                  disabled={interceptStatus !== null && !interceptStatus.supported}
                />
                <span class="label">{t("settings-intercept-copy")}</span>
              </label>
              <p class="hint">{t("settings-intercept-copy-hint")}</p>

              {#if interceptStatus && !interceptStatus.supported}
                <p class="hint">{t("settings-intercept-copy-unsupported")}</p>
              {:else if interceptStatus}
                {#if settings.shell.interceptDefaultCopy && !interceptStatus.handlerRegistered}
                  <p class="hint warn">
                    {t("settings-intercept-copy-needs-menu")}
                  </p>
                {/if}
                <button
                  type="button"
                  class="revert-copy-handler"
                  onclick={onRevertCopyHandler}
                  disabled={!interceptStatus.active}
                >
                  {t("settings-revert-copy-handler")}
                </button>
              {/if}

              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.shell.notifyOnCompletion}
                  onchange={pushSettings}
                />
                <span class="label">{t("settings-notify-completion")}</span>
              </label>
            {:else if activeTab === "secure-delete"}
              <label class="row">
                <span class="label">{t("settings-shred-method")}</span>
                <select
                  bind:value={settings.secureDelete.method}
                  onchange={pushSettings}
                >
                  <option value="zero">{t("settings-shred-zero")}</option>
                  <option value="random">{t("settings-shred-random")}</option>
                  <option value="dod-3-pass">{t("settings-shred-dod3")}</option>
                  <option value="dod-7-pass">{t("settings-shred-dod7")}</option>
                  <option value="gutmann">{t("settings-shred-gutmann")}</option>
                  <option value="nist-800-88">{t("settings-shred-nist")}</option>
                </select>
              </label>

              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.secureDelete.confirmTwice}
                  onchange={pushSettings}
                />
                <span class="label">{t("settings-shred-confirm-twice")}</span>
              </label>

              <!-- FFM-M03 — trash-aware delete safety nets. -->
              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.safety.confirmTrashDelete}
                  onchange={pushSettings}
                />
                <span class="label">{t("settings-safety-confirm-trash")}</span>
              </label>
              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.safety.moveSourceToTrash}
                  onchange={pushSettings}
                />
                <span class="label">{t("settings-safety-move-to-trash")}</span>
              </label>
              <p class="hint">{t("settings-safety-move-to-trash-hint")}</p>
            {:else if activeTab === "advanced"}
              <label class="row">
                <span class="label">{t("settings-log-level")}</span>
                <select
                  bind:value={settings.advanced.logLevel}
                  onchange={pushSettings}
                >
                  <option value="off">{t("settings-log-off")}</option>
                  <option value="error">Error</option>
                  <option value="warn">Warn</option>
                  <option value="info">Info</option>
                  <option value="debug">Debug</option>
                  <option value="trace">Trace</option>
                </select>
              </label>

              <div class="row">
                <span class="label">{t("settings-telemetry")}</span>
                <span class="muted">{t("settings-telemetry-never")}</span>
              </div>

              <label class="row">
                <span class="label">{t("settings-error-policy")}</span>
                <select
                  value={errorPolicyKind(settings.advanced.errorPolicy)}
                  onchange={(e) => setErrorPolicy((e.currentTarget as HTMLSelectElement).value)}
                >
                  <option value="ask">{t("settings-error-policy-ask")}</option>
                  <option value="skip">{t("settings-error-policy-skip")}</option>
                  <option value="retryN">{t("settings-error-policy-retry")}</option>
                  <option value="abort">{t("settings-error-policy-abort")}</option>
                </select>
              </label>

              <label class="row">
                <span class="label">{t("settings-history-retention")}</span>
                <input
                  type="number"
                  min="0"
                  max="3650"
                  bind:value={settings.advanced.historyRetentionDays}
                  onchange={pushSettings}
                />
              </label>
              <p class="hint">{t("settings-history-retention-hint")}</p>

              <div class="row">
                <span class="label">{t("settings-database-path")}</span>
                <span class="muted">
                  {settings.advanced.databasePath ?? t("settings-database-path-default")}
                </span>
              </div>

              <h4 class="subheading">{t("settings-mount-heading")}</h4>
              <p class="hint">{t("settings-mount-hint")}</p>

              {#if settings.mount}
                <label class="row check">
                  <input
                    type="checkbox"
                    bind:checked={settings.mount.mountOnLaunch}
                    onchange={pushSettings}
                  />
                  <span class="label">{t("settings-mount-on-launch")}</span>
                </label>

                <label class="row">
                  <span class="label">{t("settings-mount-on-launch-path")}</span>
                  <input
                    type="text"
                    bind:value={settings.mount.mountOnLaunchPath}
                    placeholder={t("settings-mount-on-launch-path-placeholder")}
                    disabled={!settings.mount.mountOnLaunch}
                    onchange={pushSettings}
                  />
                </label>
              {/if}

              <h4 class="subheading">{t("settings-audit-heading")}</h4>
              <p class="hint">{t("settings-audit-hint")}</p>

              {#if settings.audit}
                <label class="row check">
                  <input
                    type="checkbox"
                    bind:checked={settings.audit.enabled}
                    onchange={pushSettings}
                  />
                  <span class="label">{t("settings-audit-enable")}</span>
                </label>

                <label class="row">
                  <span class="label">{t("settings-audit-format")}</span>
                  <select
                    bind:value={settings.audit.format}
                    disabled={!settings.audit.enabled}
                    onchange={pushSettings}
                  >
                    <option value="json-lines">{t("settings-audit-format-json-lines")}</option>
                    <option value="csv">{t("settings-audit-format-csv")}</option>
                    <option value="syslog">{t("settings-audit-format-syslog")}</option>
                    <option value="cef">{t("settings-audit-format-cef")}</option>
                    <option value="leef">{t("settings-audit-format-leef")}</option>
                  </select>
                </label>

                <label class="row">
                  <span class="label">{t("settings-audit-file-path")}</span>
                  <input
                    type="text"
                    bind:value={settings.audit.filePath}
                    placeholder={t("settings-audit-file-path-placeholder")}
                    disabled={!settings.audit.enabled}
                    onchange={pushSettings}
                  />
                </label>

                <label class="row">
                  <span class="label">{t("settings-audit-max-size")}</span>
                  <input
                    type="number"
                    min="0"
                    step="1048576"
                    bind:value={settings.audit.maxSizeBytes}
                    disabled={!settings.audit.enabled}
                    onchange={pushSettings}
                  />
                </label>

                <label class="row check">
                  <input
                    type="checkbox"
                    checked={settings.audit.worm === "on"}
                    disabled={!settings.audit.enabled}
                    onchange={(e) => {
                      if (settings && settings.audit) {
                        settings.audit.worm = (e.currentTarget as HTMLInputElement).checked
                          ? "on"
                          : "off";
                        pushSettings();
                      }
                    }}
                  />
                  <span class="label">{t("settings-audit-worm")}</span>
                </label>
                <p class="hint">{t("settings-audit-worm-hint")}</p>

                <div class="row end">
                  <button
                    type="button"
                    class="secondary"
                    disabled={!settings.audit.enabled}
                    onclick={onAuditTestWrite}
                  >
                    {t("settings-audit-test-write")}
                  </button>
                  <button
                    type="button"
                    class="secondary"
                    disabled={!settings.audit.enabled}
                    onclick={onAuditVerifyChain}
                  >
                    {t("settings-audit-verify-chain")}
                  </button>
                </div>
              {/if}

              <div class="row end">
                <button class="danger" type="button" onclick={onResetAll} disabled={busy}>
                  {t("settings-reset-all")}
                </button>
              </div>
            {:else if activeTab === "updater"}
              <p class="hint">{t("settings-updater-hint")}</p>

              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.updater.autoCheck}
                  onchange={pushSettings}
                />
                <span class="label">{t("settings-updater-auto-check")}</span>
              </label>

              <label class="row">
                <span class="label">{t("settings-updater-channel")}</span>
                <select
                  bind:value={settings.updater.channel}
                  onchange={pushSettings}
                >
                  <option value="stable">{t("settings-updater-channel-stable")}</option>
                  <option value="beta">{t("settings-updater-channel-beta")}</option>
                </select>
              </label>

              <div class="row">
                <span class="label">{t("settings-updater-last-check")}</span>
                <span class="muted">
                  {formatLastCheckLabel(settings.updater.lastCheckUnixSecs)}
                </span>
              </div>

              <div class="row end">
                <button
                  type="button"
                  class="secondary"
                  onclick={onCheckForUpdatesNow}
                  disabled={checking}
                >
                  {checking
                    ? t("settings-updater-checking")
                    : t("settings-updater-check-now")}
                </button>
              </div>

              {#if lastCheck && lastCheck.availableVersion}
                <div class="row update-summary" data-tone={lastCheck.isNewer ? "available" : "up-to-date"}>
                  {#if lastCheck.isNewer}
                    <span class="label">
                      {t("settings-updater-available")} —
                      <strong>{lastCheck.availableVersion}</strong>
                    </span>
                  {:else}
                    <span class="label">{t("settings-updater-up-to-date")}</span>
                  {/if}
                </div>
                {#if lastCheck.notes}
                  <!-- Segments, not `{@html}`: the notes are remote
                       text, and rendering each piece through ordinary
                       interpolation means no markup can come out of
                       them at all. -->
                  <p class="hint notes">
                    {#each parseReleaseNotes(lastCheck.notes) as seg}
                      {#if seg.kind === "link"}
                        <button
                          type="button"
                          class="linkish"
                          title={seg.href}
                          onclick={() => openReleaseLink(seg.href)}
                        >{seg.href}</button>
                      {:else}{seg.value}{/if}
                    {/each}
                  </p>
                {/if}
                {#if lastCheck.isNewer}
                  <div class="row end">
                    <button
                      type="button"
                      class="tiny"
                      onclick={onDismissAvailable}
                    >
                      {t("settings-updater-dismiss")}
                    </button>
                  </div>
                {/if}
              {/if}

              {#if settings.updater.dismissedVersion}
                <p class="hint">
                  {t("settings-updater-dismissed")}:
                  <strong>{settings.updater.dismissedVersion}</strong>
                </p>
              {/if}
            {:else if activeTab === "network"}
              <p class="hint">{t("settings-network-hint")}</p>

              <label class="row">
                <span class="label">{t("settings-network-mode")}</span>
                <select
                  bind:value={settings.network.mode}
                  onchange={pushSettings}
                >
                  <option value="off">{t("settings-network-mode-off")}</option>
                  <option value="fixed">{t("settings-network-mode-fixed")}</option>
                  <option value="schedule">{t("settings-network-mode-schedule")}</option>
                </select>
              </label>

              {#if settings.network.mode === "fixed"}
                <label class="row">
                  <span class="label">{t("settings-network-cap-mbps")}</span>
                  <input
                    type="number"
                    min="0"
                    step="1"
                    value={Math.round(
                      settings.network.fixedBytesPerSecond / (1024 * 1024),
                    )}
                    onchange={(e) => {
                      if (!settings) return;
                      const mb = parseInt(
                        (e.currentTarget as HTMLInputElement).value,
                        10,
                      );
                      const bps = Number.isFinite(mb) && mb > 0
                        ? mb * 1024 * 1024
                        : 0;
                      settings = {
                        ...settings,
                        network: { ...settings.network, fixedBytesPerSecond: bps },
                      };
                      void pushSettings();
                    }}
                  />
                </label>
              {/if}

              {#if settings.network.mode === "schedule"}
                <label class="row stacked">
                  <span class="label">{t("settings-network-schedule")}</span>
                  <textarea
                    rows="3"
                    spellcheck={false}
                    placeholder="08:00,512k 12:00,off 13:00,512k 18:00,10M Sat-Sun,unlimited"
                    bind:value={settings.network.scheduleSpec}
                    onchange={pushSettings}
                  ></textarea>
                </label>
                <p class="hint">{t("settings-network-schedule-hint")}</p>
              {/if}

              <p class="section-header">{t("settings-network-auto-header")}</p>

              {#each [["autoOnMetered", "settings-network-auto-metered"], ["autoOnBattery", "settings-network-auto-battery"], ["autoOnCellular", "settings-network-auto-cellular"]] as const as [field, key] (field)}
                <label class="row">
                  <span class="label">{t(key)}</span>
                  <select
                    value={settings.network[field].kind}
                    onchange={(e) => {
                      if (!settings) return;
                      const kind = (e.currentTarget as HTMLSelectElement).value as
                        | "unchanged"
                        | "pause"
                        | "cap";
                      const next = kind === "cap"
                        ? { kind: "cap" as const, value: 1024 * 1024 }
                        : { kind } as { kind: "unchanged" | "pause" };
                      settings = {
                        ...settings,
                        network: { ...settings.network, [field]: next },
                      };
                      void pushSettings();
                    }}
                  >
                    <option value="unchanged">{t("settings-network-auto-unchanged")}</option>
                    <option value="pause">{t("settings-network-auto-pause")}</option>
                    <option value="cap">{t("settings-network-auto-cap")}</option>
                  </select>
                </label>
              {/each}
            {:else if activeTab === "collab"}
              <p class="hint">{t("collab-intro")}</p>
              <p class="hint">{t("collab-forward-only")}</p>

              <h4>{t("collab-members")}</h4>
              {#if !collab || collab.members.length === 0}
                <p>{t("collab-none")}</p>
              {:else}
                <ul class="collab-members">
                  {#each collab.members as m (m.label)}
                    <li>
                      <span class="member-label">{m.label}</span>
                      <code title={m.recipient}>{m.recipient}</code>
                      <button type="button" onclick={() => void collabRemove(m.label)}>
                        {t("collab-remove")}
                      </button>
                    </li>
                  {/each}
                </ul>
              {/if}
              {#if collab && collab.revoked.length > 0}
                <p class="hint">{t("collab-revoked")}: {collab.revoked.join(", ")}</p>
              {/if}

              <label>
                {t("collab-label")}
                <input bind:value={collabLabel} />
              </label>
              <label>
                {t("collab-recipient")}
                <input bind:value={collabRecipient} />
              </label>
              <button type="button" onclick={() => void collabAdd()}>
                {t("collab-add")}
              </button>
              <button type="button" onclick={() => void collabGenerate()}>
                {t("collab-generate")}
              </button>
              {#if collabNewSecret}
                <p class="hint">{t("collab-identity-once")}</p>
                <pre class="collab-secret">{collabNewSecret}</pre>
              {/if}

              <h4>{t("merge-heading")}</h4>
              <p class="hint">{t("merge-ffmpeg-hint")}</p>
              <label class="row">
                <span class="label">{t("merge-ffmpeg-enable")}</span>
                <input
                  type="checkbox"
                  bind:checked={ffmpegEnabled}
                  onchange={() => void saveFfmpegPrefs()}
                />
              </label>
              <label>
                {t("merge-ffmpeg-path")}
                <input bind:value={ffmpegPath} onchange={() => void saveFfmpegPrefs()} />
              </label>
              <p class="hint">
                {ffmpeg?.available
                  ? `${t("merge-ffmpeg-found")}: ${ffmpeg.version ?? ""}`
                  : t("merge-ffmpeg-missing")}
              </p>

              <h4>{t("collab-sas-label")}</h4>
              <p class="hint">{t("collab-sas-hint")}</p>
              <label>
                A
                <input bind:value={sasLeft} />
              </label>
              <label>
                B
                <input bind:value={sasRight} />
              </label>
              <button type="button" onclick={() => void collabComputeSas()}>
                {t("collab-sas-label")}
              </button>
              {#if sasCode}
                <pre class="collab-sas">{sasCode}</pre>
              {/if}
            {:else if activeTab === "bugreport"}
              <p class="hint">{t("bugreport-intro")}</p>

              {#if bugCtx?.pendingCrash}
                <p class="hint">{t("bugreport-pending")}</p>
              {/if}

              <label class="row stacked">
                <span class="label">{t("bugreport-description-label")}</span>
                <textarea rows="5" bind:value={bugDescription}></textarea>
              </label>

              {#if bugCtx?.pendingCrash}
                <label class="row">
                  <span class="label">{t("bugreport-include-crash")}</span>
                  <input type="checkbox" bind:checked={bugIncludeCrash} />
                </label>
              {/if}

              <button type="button" onclick={() => void refreshBugPreview()}>
                {t("bugreport-preview-label")}
              </button>
              {#if bugPreview}
                <pre class="bug-preview">{bugPreview}</pre>
              {/if}

              <div class="row">
                <button type="button" onclick={() => void sendBug("github")}>
                  {t("bugreport-send-github")}
                </button>
                <button type="button" onclick={() => void sendBug("gmail")}>
                  {t("bugreport-send-gmail")}
                </button>
                <button type="button" onclick={() => void sendBug("email")}>
                  {t("bugreport-send-email")}
                </button>
              </div>

              {#if bugCtx?.pendingCrash}
                <button type="button" onclick={() => void dismissCrash()}>
                  {t("bugreport-dismiss-crash")}
                </button>
              {/if}
              <button type="button" onclick={() => void simulateCrash()}>
                {t("bugreport-simulate")}
              </button>
            {:else if activeTab === "power"}
              <p class="hint">{t("settings-power-hint")}</p>

              <label class="row">
                <span class="label">{t("settings-power-enabled")}</span>
                <input
                  type="checkbox"
                  bind:checked={settings.power.enabled}
                  onchange={pushSettings}
                />
              </label>

              <!-- FFM-M05 — inhibit sleep while jobs run. -->
              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.power.keepAwakeDuringJobs}
                  onchange={pushSettings}
                />
                <span class="label">{t("settings-power-keep-awake")}</span>
              </label>
              <p class="hint">{t("settings-power-keep-awake-hint")}</p>

              {#each [["battery", "settings-power-battery"], ["metered", "settings-power-metered"], ["cellular", "settings-power-cellular"], ["presentation", "settings-power-presentation"], ["fullscreen", "settings-power-fullscreen"]] as const as [field, key] (field)}
                <label class="row">
                  <span class="label">{t(key)}</span>
                  <select
                    value={settings.power[field].kind}
                    disabled={!settings.power.enabled}
                    onchange={(e) => {
                      if (!settings) return;
                      const kind = (e.currentTarget as HTMLSelectElement).value as
                        | "continue"
                        | "pause";
                      settings = {
                        ...settings,
                        power: { ...settings.power, [field]: { kind } },
                      };
                      void pushSettings();
                    }}
                  >
                    <option value="continue">{t("settings-power-continue")}</option>
                    <option value="pause">{t("settings-power-pause")}</option>
                  </select>
                </label>
              {/each}

              <label class="row">
                <span class="label">{t("settings-power-thermal")}</span>
                <select
                  value={settings.power.thermal.kind}
                  disabled={!settings.power.enabled}
                  onchange={(e) => {
                    if (!settings) return;
                    const kind = (e.currentTarget as HTMLSelectElement).value as
                      | "continue"
                      | "pause";
                    settings = {
                      ...settings,
                      power: { ...settings.power, thermal: { kind } },
                    };
                    void pushSettings();
                  }}
                >
                  <option value="continue">{t("settings-power-continue")}</option>
                  <option value="pause">{t("settings-power-pause")}</option>
                </select>
              </label>
            {:else if activeTab === "remotes"}
              <RemotesTab />
            {:else if activeTab === "mobile"}
              <MobilePanel bind:settings />
            {:else if activeTab === "provenance"}
              <ProvenanceTab />
            {:else if activeTab === "sanitize"}
              <SanitizeTab />
            {:else if activeTab === "plugins"}
              <PluginsTab />
            {:else if activeTab === "server"}
              <p class="hint">{t("server-hint")}</p>

              <h4 class="subheading">{t("server-protocols")}</h4>
              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.server.webdav}
                  onchange={pushSettings}
                />
                <span class="label">WebDAV</span>
              </label>
              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.server.http}
                  onchange={pushSettings}
                />
                <span class="label">HTTP</span>
              </label>
              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.server.s3}
                  onchange={pushSettings}
                />
                <span class="label">S3</span>
              </label>
              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.server.sftp}
                  onchange={pushSettings}
                />
                <span class="label">SFTP</span>
              </label>

              <label class="row">
                <span class="label">{t("server-bind-addr")}</span>
                <input
                  type="text"
                  bind:value={settings.server.bindAddr}
                  onchange={pushSettings}
                  placeholder="127.0.0.1:8080"
                  spellcheck={false}
                />
              </label>

              <label class="row">
                <span class="label">{t("server-root")}</span>
                <input
                  type="text"
                  bind:value={settings.server.root}
                  onchange={pushSettings}
                  placeholder="."
                  spellcheck={false}
                />
                <button type="button" class="secondary" onclick={onPickServerRoot}>
                  …
                </button>
              </label>

              <label class="row check">
                <input
                  type="checkbox"
                  bind:checked={settings.server.readonly}
                  onchange={pushSettings}
                />
                <span class="label">{t("server-readonly")}</span>
              </label>

              <label class="row">
                <span class="label">{t("server-auth-mode")}</span>
                <select
                  bind:value={settings.server.auth.mode}
                  onchange={pushSettings}
                >
                  <option value="none">{t("server-auth-none")}</option>
                  <option value="bearer">{t("server-auth-bearer")}</option>
                  <option value="basic">{t("server-auth-basic")}</option>
                </select>
              </label>

              {#if settings.server.auth.mode === "bearer"}
                <label class="row">
                  <span class="label">{t("server-auth-token")}</span>
                  <input
                    type="password"
                    bind:value={settings.server.auth.token}
                    onchange={pushSettings}
                    spellcheck={false}
                  />
                </label>
              {:else if settings.server.auth.mode === "basic"}
                <label class="row">
                  <span class="label">{t("server-auth-user")}</span>
                  <input
                    type="text"
                    bind:value={settings.server.auth.user}
                    onchange={pushSettings}
                    spellcheck={false}
                  />
                </label>
                <label class="row">
                  <span class="label">{t("server-auth-password")}</span>
                  <input
                    type="password"
                    bind:value={settings.server.auth.password}
                    onchange={pushSettings}
                    spellcheck={false}
                  />
                </label>
              {/if}

              <label class="row">
                <span class="label">{t("otel-endpoint")}</span>
                <input
                  type="text"
                  bind:value={settings.server.otelEndpoint}
                  onchange={pushSettings}
                  placeholder="http://localhost:4318/v1/traces"
                  spellcheck={false}
                />
              </label>

              <h4 class="subheading">{t("webhook-section")}</h4>
              {#if settings.server.webhooks.length === 0}
                <p class="hint empty">{t("webhook-empty")}</p>
              {:else}
                <ul class="webhook-list">
                  {#each settings.server.webhooks as hook, i (i)}
                    <li class="webhook-row">
                      <select
                        bind:value={hook.target}
                        onchange={pushSettings}
                      >
                        <option value="slack">Slack</option>
                        <option value="discord">Discord</option>
                        <option value="ntfy">ntfy</option>
                        <option value="pushover">Pushover</option>
                      </select>
                      <input
                        type="text"
                        class="webhook-url"
                        bind:value={hook.url}
                        onchange={pushSettings}
                        placeholder={t("webhook-url")}
                        spellcheck={false}
                      />
                      <button
                        type="button"
                        class="tiny danger"
                        onclick={() => onRemoveWebhook(i)}
                      >
                        {t("webhook-remove")}
                      </button>
                      {#if hook.target === "pushover"}
                        <input
                          type="text"
                          class="webhook-extra"
                          bind:value={hook.pushoverToken}
                          onchange={pushSettings}
                          placeholder={t("webhook-pushover-token")}
                          spellcheck={false}
                        />
                        <input
                          type="text"
                          class="webhook-extra"
                          bind:value={hook.pushoverUser}
                          onchange={pushSettings}
                          placeholder={t("webhook-pushover-user")}
                          spellcheck={false}
                        />
                      {/if}
                    </li>
                  {/each}
                </ul>
              {/if}
              <div class="row">
                <button type="button" class="secondary" onclick={onAddWebhook}>
                  {t("webhook-add")}
                </button>
              </div>

              <div class="row end">
                {#if serverState?.running}
                  <button
                    type="button"
                    class="danger"
                    onclick={onServerStop}
                    disabled={serverBusy}
                  >
                    {t("server-stop")}
                  </button>
                {:else}
                  <button
                    type="button"
                    class="secondary"
                    onclick={onServerStart}
                    disabled={serverBusy}
                  >
                    {t("server-start")}
                  </button>
                {/if}
              </div>

              <div class="row server-status" data-running={serverState?.running ?? false}>
                {#if serverState?.running}
                  <span class="label"
                    >{t("server-status-running", {
                      addr: serverState.boundAddr ?? "",
                    })}</span
                  >
                {:else}
                  <span class="label">{t("server-status-stopped")}</span>
                {/if}
              </div>

              {#if serverState?.running && serverState.metricsUrl}
                <div class="row">
                  <span class="label">{t("server-metrics-url")}</span>
                  <span class="muted">{serverState.metricsUrl}</span>
                </div>
              {/if}
            {:else if activeTab === "schedules"}
              <SchedulerPanel />
            {:else if activeTab === "queues"}
              <QueueAffinityPanel />
            {:else if activeTab === "favorites"}
              <FavoritesPanel />
            {:else if activeTab === "profiles"}
              <p class="hint">{t("settings-profiles-hint")}</p>
              <div class="row">
                <input
                  type="text"
                  placeholder={t("settings-profile-name-placeholder")}
                  bind:value={profileNameInput}
                />
                <button type="button" class="secondary" onclick={onSaveProfile} disabled={busy || !profileNameInput.trim()}>
                  {t("settings-profile-save")}
                </button>
                <button type="button" class="secondary" onclick={onImportProfile} disabled={busy}>
                  {t("settings-profile-import")}
                </button>
              </div>

              {#if profiles.length === 0}
                <p class="empty">{t("settings-profile-empty")}</p>
              {:else}
                <ul class="profile-list">
                  {#each profiles as p (p.name)}
                    <li>
                      <span class="profile-name" title={p.path}>{p.name}</span>
                      <button
                        type="button"
                        class="tiny"
                        onclick={() => onLoadProfile(p.name)}
                        disabled={busy}
                      >
                        {t("settings-profile-load")}
                      </button>
                      <button
                        type="button"
                        class="tiny"
                        onclick={() => onExportProfile(p.name)}
                        disabled={busy}
                      >
                        {t("settings-profile-export")}
                      </button>
                      <button
                        type="button"
                        class="tiny danger"
                        onclick={() => onDeleteProfile(p.name)}
                        disabled={busy}
                      >
                        {t("settings-profile-delete")}
                      </button>
                    </li>
                  {/each}
                </ul>
              {/if}
            {/if}
          </div>
        </div>
      {/if}
    </div>
    {/key}
  </div>
{/if}

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.36);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 92;
  }

  .modal {
    width: min(720px, 96vw);
    max-height: 86vh;
    padding: 12px 14px 14px;
    background: var(--surface, #ffffff);
    color: var(--fg, #1f1f1f);
    border: 1px solid var(--border, rgba(128, 128, 128, 0.3));
    border-radius: 10px;
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.24);
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding-bottom: 4px;
    border-bottom: 1px solid var(--border, rgba(128, 128, 128, 0.2));
  }

  h2 {
    margin: 0;
    font-size: 14px;
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

  .close:hover {
    background: var(--hover, rgba(128, 128, 128, 0.14));
  }

  .loading {
    padding: 24px 16px;
    color: var(--muted, #6a6a6a);
    font-size: 13px;
    text-align: center;
  }

  .body {
    display: flex;
    gap: 14px;
    min-height: 280px;
    max-height: calc(86vh - 48px);
    overflow: hidden;
  }

  .tabs {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 160px;
    padding-right: 10px;
    border-right: 1px solid var(--border, rgba(128, 128, 128, 0.18));
  }

  .tabs button {
    text-align: left;
    padding: 6px 10px;
    background: transparent;
    border: 1px solid transparent;
    border-radius: 4px;
    color: inherit;
    font: inherit;
    font-size: 12px;
    cursor: pointer;
  }

  .tabs button:hover:not(.active) {
    background: var(--hover, rgba(128, 128, 128, 0.12));
  }

  .tabs button.active {
    background: var(--row-selected, rgba(79, 140, 255, 0.12));
    color: var(--accent, #4f8cff);
    font-weight: 600;
  }

  .tabpanel {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 2px 6px 2px 2px;
    overflow-y: auto;
    outline: none;
  }

  .tabpanel:focus-visible {
    outline: 2px solid var(--accent, #4f8cff);
    outline-offset: 2px;
  }

  .row {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 12px;
    flex-wrap: wrap;
  }

  .row.check {
    justify-content: flex-start;
  }

  .row.end {
    justify-content: flex-end;
    margin-top: 10px;
  }

  .row .label {
    min-width: 140px;
    color: var(--fg-dim, #6a6a6a);
  }

  /* `stacked` / `column` were used in the markup but never defined, and
     no rule ever gave a textarea a width — so every multi-line field
     fell back to the browser's ~20-column default (167px inside a 529px
     pane) with its caption stranded on the baseline beside it. */
  .row.stacked {
    flex-direction: column;
    align-items: stretch;
    gap: 4px;
  }

  .row.stacked > .label {
    min-width: 0;
  }

  /* Chrome copied from the `.row select / input` group rather than
     joining it: that rule sets `flex: 1`, which grows a control along
     the container's main axis. In a stacked row that axis is vertical,
     so it would stretch the textarea instead of widening it. */
  .tabpanel textarea {
    width: 100%;
    box-sizing: border-box;
    resize: vertical;
    padding: 4px 6px;
    font: inherit;
    font-size: 12px;
    border: 1px solid var(--border, rgba(128, 128, 128, 0.3));
    border-radius: 4px;
    background: var(--surface, #ffffff);
    color: inherit;
  }

  /* Phase 51 — collaborator rows. A recipient is a 62-character age1
     key with no break opportunity, so with no track to sit in it forced
     Remove onto a line of its own and rows stopped matching each other.
     Fixed label + elastic key + natural-width button keeps every row on
     one line; the full key stays reachable via the title tooltip. */
  .collab-members {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .collab-members li {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .collab-members .member-label,
  .collab-members code {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .collab-members .member-label {
    flex: 0 0 120px;
  }

  .collab-members code {
    flex: 1;
    min-width: 0;
  }

  .collab-members button {
    flex: 0 0 auto;
  }

  .row select,
  .row input[type="number"],
  .row input[type="text"] {
    flex: 1;
    padding: 4px 6px;
    font: inherit;
    font-size: 12px;
    border: 1px solid var(--border, rgba(128, 128, 128, 0.3));
    border-radius: 4px;
    background: var(--surface, #ffffff);
    color: inherit;
  }

  /* A link rendered as a button so it cannot navigate the webview —
     the click handler hands the URL to the OS browser instead. Styled
     to read as a link because that is what it is to the user. */
  .linkish {
    background: none;
    border: 0;
    padding: 0;
    font: inherit;
    color: var(--accent, #2563eb);
    text-decoration: underline;
    cursor: pointer;
    word-break: break-all;
    text-align: left;
  }
  .linkish:hover,
  .linkish:focus-visible {
    text-decoration-thickness: 2px;
  }
  .hint {
    margin: 0;
    font-size: 11px;
    color: var(--fg-dim, #6a6a6a);
  }

  /* FFM-M01 — warn variant for the "context menu not installed" note. */
  .hint.warn {
    color: var(--warn, #e4a040);
  }

  .revert-copy-handler {
    align-self: flex-start;
    margin-top: 4px;
  }

  .subheading {
    margin: 10px 0 4px;
    font-size: 12px;
    font-weight: 600;
    color: var(--fg-dim, #6a6a6a);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }

  .empty {
    padding: 12px 0;
    color: var(--muted, #6a6a6a);
    font-size: 12px;
    text-align: center;
  }

  /* Phase 45.6 — tray-pinned destinations panel. List rows show
     label + path + a delete button; the add row sits below. */
  .tray-pin-list {
    list-style: none;
    margin: 8px 0;
    padding: 0;
    border: 1px solid var(--border, rgba(128, 128, 128, 0.18));
    border-radius: 4px;
    overflow: hidden;
  }

  .tray-pin-row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 8px;
    background: var(--surface, #ffffff);
    border-bottom: 1px solid var(--border, rgba(128, 128, 128, 0.12));
  }

  .tray-pin-row:last-child {
    border-bottom: none;
  }

  .tray-pin-label {
    flex: 0 0 auto;
    min-width: 90px;
    font-weight: 600;
    color: var(--fg-strong, #1f1f1f);
  }

  .tray-pin-path {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--fg-dim, #6a6a6a);
    font-family: var(--mono, ui-monospace, SFMono-Regular, Menlo, monospace);
    font-size: 11px;
  }

  .tray-pin-remove {
    flex: 0 0 auto;
    width: 24px;
    height: 24px;
    padding: 0;
    background: transparent;
    border: 1px solid transparent;
    border-radius: 4px;
    color: var(--fg-dim, #6a6a6a);
    font: inherit;
    font-size: 16px;
    line-height: 1;
    cursor: pointer;
  }

  .tray-pin-remove:hover {
    color: var(--error, #c24141);
    border-color: var(--error, #c24141);
  }

  .tray-pin-add {
    display: flex;
    gap: 6px;
    align-items: center;
    margin-top: 4px;
  }

  .tray-pin-add input[type="text"] {
    flex: 1;
    min-width: 0;
  }

  .tray-pin-add input[type="text"]:first-of-type {
    flex: 0 0 120px;
  }

  .tray-pin-pick {
    flex: 0 0 auto;
    width: 28px;
    padding: 4px 0;
    background: var(--surface, #ffffff);
    border: 1px solid var(--border, rgba(128, 128, 128, 0.3));
    border-radius: 4px;
    color: inherit;
    font: inherit;
    cursor: pointer;
  }

  .tray-pin-add-btn {
    flex: 0 0 auto;
    padding: 4px 12px;
    background: var(--accent, #4f8cff);
    color: #ffffff;
    border: 1px solid var(--accent, #4f8cff);
    border-radius: 4px;
    font: inherit;
    font-size: 12px;
    cursor: pointer;
  }

  .tray-pin-add-btn:hover {
    filter: brightness(1.05);
  }

  .muted {
    color: var(--fg-dim, #6a6a6a);
    font-family: var(--mono, ui-monospace, SFMono-Regular, Menlo, monospace);
    font-size: 11px;
    overflow-wrap: anywhere;
  }

  button {
    font-size: 12px;
    padding: 5px 12px;
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

  button.tiny {
    padding: 3px 8px;
    font-size: 11px;
  }

  button.danger {
    border-color: var(--error, #d95757);
    color: var(--error, #c24141);
  }

  button.danger:hover:not(:disabled) {
    background: rgba(217, 87, 87, 0.08);
  }

  .profile-list {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .profile-list li {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 8px;
    border: 1px solid var(--border, rgba(128, 128, 128, 0.18));
    border-radius: 4px;
    background: var(--surface-alt, rgba(0, 0, 0, 0.02));
  }

  .profile-name {
    flex: 1;
    font-weight: 500;
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  /* Phase 48 — Settings → Server webhook list. One row per
     destination: service dropdown + URL + remove; Pushover rows wrap
     their token/user fields onto the next line. */
  .webhook-list {
    list-style: none;
    margin: 6px 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .webhook-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    padding: 6px 8px;
    border: 1px solid var(--border, rgba(128, 128, 128, 0.18));
    border-radius: 4px;
    background: var(--surface-alt, rgba(0, 0, 0, 0.02));
  }

  .webhook-row .webhook-url {
    flex: 1;
    min-width: 140px;
  }

  .webhook-row .webhook-extra {
    flex: 1 1 100%;
  }

  .server-status .label {
    min-width: 0;
    color: var(--fg-dim, #6a6a6a);
  }

  .server-status[data-running="true"] .label {
    color: var(--accent, #4f8cff);
    font-weight: 600;
  }

  /* Phase 24 — Security-metadata subsection. The header row carries
     the master toggle; nested toggles indent under it and only render
     when the master is on so a user opting out doesn't have to look at
     five irrelevant sub-checkboxes. */
  .meta-subsection {
    margin-top: 8px;
    padding: 8px 0 4px 0;
    border-top: 1px solid var(--border, rgba(127, 127, 127, 0.2));
  }
  .meta-subsection .section-header {
    font-weight: 600;
  }
  .meta-child {
    padding-left: 18px;
  }
  .motw-warning {
    color: var(--warning, #d97706);
    margin-left: 4px;
    font-size: 0.9em;
  }
</style>
