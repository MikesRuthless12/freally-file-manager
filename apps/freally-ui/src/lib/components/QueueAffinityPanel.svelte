<script lang="ts">
  // FFM-M18 — Settings → Drive queues. Physical-drive detection is a
  // heuristic and it is sometimes wrong: a VHDX mounted as `T:` is
  // really backed by `C:`, so two queues end up thrashing one spindle.
  //
  // A group is a name plus a set of folders that share one queue.
  // Listing two folders in one group force-merges them; giving each its
  // own group force-splits paths the probe wrongly lumped together.
  import { onMount } from "svelte";

  import { errorText } from "../errors";
  import { t } from "../i18n";
  import { pickFolders, queueGetAffinity, queueSetAffinity } from "../ipc";
  import type { AffinityGroupDto } from "../types";
  import Icon from "../icons/Icon.svelte";

  let groups = $state<AffinityGroupDto[]>([]);
  let busy = $state(false);
  let error = $state("");

  async function load() {
    try {
      groups = await queueGetAffinity();
    } catch (e) {
      error = errorText(e);
    }
  }

  /** A group with no folders yet is mid-edit, not invalid — pushing it
   *  would fail validation and show the user an error for a group they
   *  are still filling in. */
  function isComplete(g: AffinityGroupDto): boolean {
    return g.name.trim().length > 0 && g.prefixes.length > 0;
  }

  async function push() {
    if (!groups.every(isComplete)) return;
    busy = true;
    error = "";
    try {
      groups = await queueSetAffinity($state.snapshot(groups));
    } catch (e) {
      error = errorText(e);
      // Re-read so the panel shows what is actually in force rather
      // than the rejected edit.
      await load();
    } finally {
      busy = false;
    }
  }

  function addGroup() {
    groups = [...groups, { name: "", prefixes: [], workers: 0 }];
  }

  function removeGroup(index: number) {
    groups = groups.filter((_, i) => i !== index);
    void push();
  }

  async function addPath(index: number) {
    const picked = await pickFolders();
    if (picked.length === 0) return;
    groups[index].prefixes = [...groups[index].prefixes, ...picked];
    await push();
  }

  function removePath(groupIndex: number, pathIndex: number) {
    groups[groupIndex].prefixes = groups[groupIndex].prefixes.filter(
      (_, i) => i !== pathIndex,
    );
    void push();
  }

  onMount(load);
</script>

<section class="panel" aria-labelledby="affinity-heading">
  <h3 id="affinity-heading">{t("settings-affinity-title")}</h3>
  <p class="hint">{t("settings-affinity-description")}</p>

  {#if error}
    <p class="error" role="alert">{error}</p>
  {/if}

  <ul class="groups">
    {#each groups as group, gi (gi)}
      <li>
        <div class="group-head">
          <label class="row">
            <span class="label">{t("settings-affinity-name")}</span>
            <input type="text" bind:value={group.name} onblur={push} />
          </label>
          <label class="row">
            <span class="label">{t("settings-affinity-workers")}</span>
            <select bind:value={group.workers} onchange={push}>
              <option value={0}>{t("settings-affinity-workers-auto")}</option>
              {#each [1, 2, 4, 8, 16] as n (n)}
                <option value={n}>{n}</option>
              {/each}
            </select>
          </label>
          <button
            type="button"
            onclick={() => removeGroup(gi)}
            disabled={busy}
            aria-label={`${t("action-remove")}: ${group.name}`}
          >
            {t("action-remove")}
          </button>
        </div>

        <span class="label">{t("settings-affinity-paths")}</span>
        <ul class="paths">
          {#each group.prefixes as prefix, pi (pi)}
            <li>
              <span class="path" title={prefix}>{prefix}</span>
              <button
                type="button"
                onclick={() => removePath(gi, pi)}
                disabled={busy}
                aria-label={`${t("action-remove")}: ${prefix}`}
              >
                <Icon name="x" size={12} />
              </button>
            </li>
          {/each}
        </ul>
        <button type="button" onclick={() => addPath(gi)} disabled={busy}>
          {t("settings-affinity-path-add")}
        </button>
      </li>
    {/each}
  </ul>

  <button type="button" class="primary" onclick={addGroup} disabled={busy}>
    {t("settings-affinity-add")}
  </button>
</section>

<style>
  .panel {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  .hint {
    opacity: 0.75;
    font-size: 0.9em;
    margin: 0;
  }
  .groups,
  .paths {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  .groups > li {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    padding: 0.6rem;
    border: 1px solid var(--border, #3a3a3a);
    border-radius: 6px;
  }
  .group-head {
    display: flex;
    gap: 0.75rem;
    align-items: center;
    flex-wrap: wrap;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }
  .paths > li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
  }
  .path {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono, monospace);
    font-size: 0.85em;
  }
  .error {
    color: var(--error, #e05252);
  }
</style>
