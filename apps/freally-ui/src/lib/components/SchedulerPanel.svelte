<script lang="ts">
  // FFM-M17 — Settings → Schedules. Creates, lists, edits, and removes
  // user-level scheduled runs. Every row is a real OS artifact: a Task
  // Scheduler task, a LaunchAgent, or a systemd --user timer.
  //
  // A schedule runs this app's own `--enqueue` CLI over a saved file
  // list (FFM-M13), so the panel asks for a manifest + destination
  // rather than a command line — the backend never accepts a program
  // name from the renderer.
  import { onMount } from "svelte";

  import { errorText } from "../errors";
  import { t } from "../i18n";
  import {
    scheduleList,
    scheduleRemove,
    scheduleSave,
    pickFiles,
    pickFolders,
  } from "../ipc";
  import type { ScheduleDto } from "../types";

  let rows = $state<ScheduleDto[]>([]);
  let busy = $state(false);
  let error = $state("");
  let draft = $state<ScheduleDto | null>(null);

  function blankDraft(): ScheduleDto {
    return {
      id: "",
      label: "",
      verb: "copy",
      filesFrom: "",
      destination: "",
      relativeTo: "",
      trigger: "daily",
      weekday: 0,
      hour: 3,
      minute: 0,
      runWhenAvailable: true,
    };
  }

  async function load() {
    try {
      rows = await scheduleList();
    } catch (e) {
      error = errorText(e);
    }
  }

  function startAdd() {
    draft = blankDraft();
    error = "";
  }

  function startEdit(row: ScheduleDto) {
    draft = { ...row };
    error = "";
  }

  async function save() {
    if (draft === null) return;
    busy = true;
    error = "";
    try {
      // A blank id means "create" — the backend mints a unique one
      // from the label. Deriving it here would silently overwrite an
      // existing schedule whenever two shared a label.
      rows = await scheduleSave($state.snapshot(draft));
      draft = null;
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }

  async function remove(id: string) {
    busy = true;
    error = "";
    try {
      rows = await scheduleRemove(id);
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }

  async function browseManifest() {
    const picked = await pickFiles();
    if (picked.length > 0 && draft !== null) draft.filesFrom = picked[0];
  }

  async function browseDestination() {
    const picked = await pickFolders();
    if (picked.length > 0 && draft !== null) draft.destination = picked[0];
  }

  /** Render a Unix-seconds preview in the user's own locale. */
  function formatNextRun(secs: number | null | undefined): string {
    if (secs === null || secs === undefined) return "—";
    return new Date(secs * 1000).toLocaleString();
  }

  function triggerLabel(row: ScheduleDto): string {
    if (row.trigger === "hourly") return t("settings-schedule-trigger-hourly");
    if (row.trigger === "weekly") return t("settings-schedule-trigger-weekly");
    return t("settings-schedule-trigger-daily");
  }

  onMount(load);
</script>

<section class="panel" aria-labelledby="schedules-heading">
  <h3 id="schedules-heading">{t("settings-schedules-title")}</h3>

  {#if error}
    <p class="error" role="alert">{error}</p>
  {/if}

  {#if rows.length === 0 && draft === null}
    <p class="empty">{t("settings-schedules-empty")}</p>
  {/if}

  {#if rows.length > 0}
    <ul class="rows">
      {#each rows as row (row.id)}
        <li>
          <div class="row-main">
            <span class="row-label">{row.label}</span>
            <span class="row-meta">
              {triggerLabel(row)} ·
              {t("settings-schedule-next-run")}: {formatNextRun(
                row.nextRunUnixSecs,
              )}
            </span>
            {#if row.installed === false}
              <span class="warn" role="status"
                >{t("settings-schedule-not-installed")}</span
              >
            {/if}
            {#if row.missedRunHonored === false}
              <span class="note"
                >{t("settings-schedule-missed-unsupported")}</span
              >
            {/if}
          </div>
          <div class="row-actions">
            <button
              type="button"
              onclick={() => startEdit(row)}
              disabled={busy}
              aria-label={`${t("action-edit")}: ${row.label}`}
            >
              {t("action-edit")}
            </button>
            <button
              type="button"
              onclick={() => remove(row.id)}
              disabled={busy}
              aria-label={`${t("action-remove")}: ${row.label}`}
            >
              {t("action-remove")}
            </button>
          </div>
        </li>
      {/each}
    </ul>
  {/if}

  {#if draft === null}
    <button type="button" class="primary" onclick={startAdd} disabled={busy}>
      {t("settings-schedule-add")}
    </button>
  {:else}
    <div class="editor">
      <label class="row">
        <span class="label">{t("settings-schedule-name")}</span>
        <input
          type="text"
          bind:value={draft.label}
          placeholder={t("settings-schedules-title")}
        />
      </label>

      <label class="row">
        <span class="label">{t("settings-schedule-verb")}</span>
        <select bind:value={draft.verb}>
          <option value="copy">{t("settings-schedule-verb-copy")}</option>
          <option value="move">{t("settings-schedule-verb-move")}</option>
        </select>
      </label>

      <div class="row">
        <span class="label">{t("settings-schedule-manifest")}</span>
        <input type="text" bind:value={draft.filesFrom} />
        <button type="button" onclick={browseManifest}>
          {t("action-browse")}
        </button>
      </div>

      <div class="row">
        <span class="label">{t("settings-schedule-destination")}</span>
        <input type="text" bind:value={draft.destination} />
        <button type="button" onclick={browseDestination}>
          {t("action-browse")}
        </button>
      </div>

      <label class="row">
        <span class="label">{t("settings-schedule-when")}</span>
        <select bind:value={draft.trigger}>
          <option value="hourly"
            >{t("settings-schedule-trigger-hourly")}</option
          >
          <option value="daily">{t("settings-schedule-trigger-daily")}</option>
          <option value="weekly"
            >{t("settings-schedule-trigger-weekly")}</option
          >
        </select>
      </label>

      {#if draft.trigger === "weekly"}
        <label class="row">
          <span class="label">{t("settings-schedule-weekday")}</span>
          <input type="number" min="0" max="6" bind:value={draft.weekday} />
        </label>
      {/if}

      {#if draft.trigger !== "hourly"}
        <label class="row">
          <span class="label">{t("settings-schedule-hour")}</span>
          <input type="number" min="0" max="23" bind:value={draft.hour} />
        </label>
      {/if}

      <label class="row">
        <span class="label">{t("settings-schedule-minute")}</span>
        <input type="number" min="0" max="59" bind:value={draft.minute} />
      </label>

      <label class="row check">
        <input type="checkbox" bind:checked={draft.runWhenAvailable} />
        <span class="label">{t("settings-schedule-missed")}</span>
      </label>

      <div class="editor-actions">
        <button type="button" class="primary" onclick={save} disabled={busy}>
          {t("action-save")}
        </button>
        <button type="button" onclick={() => (draft = null)} disabled={busy}>
          {t("action-cancel")}
        </button>
      </div>
    </div>
  {/if}
</section>

<style>
  .panel {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  .rows {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .rows li {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 0.75rem;
    padding: 0.5rem 0.65rem;
    border: 1px solid var(--border, #3a3a3a);
    border-radius: 6px;
  }
  .row-main {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    min-width: 0;
  }
  .row-label {
    font-weight: 600;
  }
  .row-meta,
  .note {
    font-size: 0.85em;
    opacity: 0.75;
  }
  .warn {
    font-size: 0.85em;
    color: var(--warn, #d08b26);
  }
  .row-actions {
    display: flex;
    gap: 0.35rem;
    flex-shrink: 0;
  }
  .editor {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    padding: 0.65rem;
    border: 1px solid var(--border, #3a3a3a);
    border-radius: 6px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .row .label {
    min-width: 9rem;
  }
  .row input[type="text"] {
    flex: 1;
    min-width: 0;
  }
  .editor-actions {
    display: flex;
    gap: 0.5rem;
  }
  .empty {
    opacity: 0.7;
  }
  .error {
    color: var(--error, #e05252);
  }
</style>
