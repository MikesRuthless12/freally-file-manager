<!--
  Phase 49 — Library drawer.

  A unified view over the content-addressed chunk Repository, behind the
  Footer's "Library" button. Additive: the History / Sync / Totals
  drawers are unchanged. Three sub-views plus a dedup "hero":

  - Live      — the repository at a glance (stored vs effective, chunks).
  - Snapshots — the unified snapshot timeline (copy / sync / version /
                backup), newest first.
  - Versions  — per-file rolling versions (Phase 42), looked up by path.

  Reads are advisory: a "repository-unavailable" error renders the
  empty/unavailable state rather than surfacing a toast.
-->
<script lang="ts">
  import Icon from "../icons/Icon.svelte";
  import { i18nVersion, t } from "../i18n";
  import { formatBytes } from "../format";
  import {
    listVersions,
    repositorySnapshots,
    repositoryStats,
    type RepositorySnapshotDto,
    type RepositoryStatsDto,
    type VersionRecordDto,
  } from "../ipc";
  import { closeLibraryDrawer, libraryDrawerOpen, pushToast } from "../stores";

  type SubView = "live" | "snapshots" | "versions";
  const SUB_VIEWS: SubView[] = ["live", "snapshots", "versions"];

  let subView = $state<SubView>("live");
  let stats = $state<RepositoryStatsDto | null>(null);
  let snapshots = $state<RepositorySnapshotDto[]>([]);
  let versions = $state<VersionRecordDto[]>([]);
  let versionPath = $state("");
  let unavailable = $state(false);

  // Refresh stats + snapshots whenever the drawer opens.
  $effect(() => {
    if ($libraryDrawerOpen) {
      void refresh();
    }
  });

  async function refresh() {
    unavailable = false;
    try {
      const [s, snaps] = await Promise.all([
        repositoryStats(),
        repositorySnapshots(),
      ]);
      stats = s;
      // Newest first for the timeline.
      snapshots = snaps.slice().reverse();
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      if (msg === "repository-unavailable") {
        unavailable = true;
      } else {
        pushToast("error", msg);
      }
    }
  }

  async function loadVersions() {
    const path = versionPath.trim();
    if (!path) {
      versions = [];
      return;
    }
    try {
      versions = await listVersions(path);
    } catch (e) {
      pushToast("error", e instanceof Error ? e.message : String(e));
    }
  }

  function fmtDate(ms: number): string {
    return new Date(ms).toLocaleString();
  }

  function savedPct(ratio: number): string {
    return `${Math.round(ratio * 100)}%`;
  }
</script>

{#if $libraryDrawerOpen}
  <aside class="drawer" aria-label={t("library-title")}>
    {#key $i18nVersion}
      <header>
        <h2>{t("library-title")}</h2>
        <button
          class="close"
          type="button"
          aria-label={t("action-close")}
          onclick={closeLibraryDrawer}
        >
          <Icon name="x" size={16} />
        </button>
      </header>

      {#if unavailable}
        <p class="notice">{t("library-unavailable")}</p>
      {:else}
        {#if stats}
          <div class="hero">
            <span class="hero-num">💾 {formatBytes(stats.storedBytes)}</span>
            <span class="hero-sub">
              {#if stats.effectiveBytes > 0}
                {t("library-hero-savings", {
                  effective: formatBytes(stats.effectiveBytes),
                  pct: savedPct(stats.savedRatio),
                })}
              {:else}
                {t("library-hero-empty", { chunks: stats.chunkCount })}
              {/if}
            </span>
          </div>
        {/if}

        <div class="tabs" role="tablist">
          {#each SUB_VIEWS as id (id)}
            <button
              type="button"
              role="tab"
              class="tab"
              class:active={subView === id}
              aria-selected={subView === id}
              onclick={() => (subView = id)}
            >
              {t(`library-tab-${id}`)}
            </button>
          {/each}
        </div>

        {#if subView === "live"}
          {#if stats}
            <dl class="grid">
              <div>
                <dt>{t("library-stat-stored")}</dt>
                <dd>{formatBytes(stats.storedBytes)}</dd>
              </div>
              <div>
                <dt>{t("library-stat-effective")}</dt>
                <dd>{formatBytes(stats.effectiveBytes)}</dd>
              </div>
              <div>
                <dt>{t("library-stat-snapshots")}</dt>
                <dd>{stats.snapshotCount}</dd>
              </div>
              <div>
                <dt>{t("library-stat-chunks")}</dt>
                <dd>{stats.chunkCount}</dd>
              </div>
            </dl>
          {:else}
            <p class="empty">{t("library-loading")}</p>
          {/if}
        {:else if subView === "snapshots"}
          {#if snapshots.length === 0}
            <p class="empty">{t("library-snapshot-empty")}</p>
          {:else}
            <ul class="list">
              {#each snapshots as s (s.id)}
                <li>
                  <span class="kind kind-{s.kind}">{t(`repo-kind-${s.kind}`)}</span>
                  <span class="label">{s.label}</span>
                  <span class="meta">
                    {fmtDate(s.createdAtMs)} ·
                    {t("library-snapshot-files", { n: s.fileCount })} ·
                    {formatBytes(s.totalSize)}
                  </span>
                </li>
              {/each}
            </ul>
          {/if}
        {:else}
          <div class="version-bar">
            <input
              type="text"
              placeholder={t("library-version-path-ph")}
              bind:value={versionPath}
              onkeydown={(e) => e.key === "Enter" && loadVersions()}
            />
            <button type="button" onclick={loadVersions}>
              {t("library-version-load")}
            </button>
          </div>
          {#if versions.length === 0}
            <p class="empty">{t("library-version-empty")}</p>
          {:else}
            <ul class="list">
              {#each versions as v (v.rowId)}
                <li>
                  <span class="label">{fmtDate(v.tsMs)}</span>
                  <span class="meta">{formatBytes(v.size)}</span>
                </li>
              {/each}
            </ul>
          {/if}
        {/if}
      {/if}
    {/key}
  </aside>
{/if}

<style>
  .drawer {
    position: fixed;
    inset: 0;
    background: var(--surface, #ffffff);
    color: var(--fg, #1f1f1f);
    display: flex;
    flex-direction: column;
    overflow-y: auto;
    z-index: 85;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 16px;
    border-bottom: 1px solid var(--border, rgba(128, 128, 128, 0.3));
  }
  h2 {
    margin: 0;
    font-size: 1.1rem;
  }
  .close {
    background: none;
    border: none;
    cursor: pointer;
    color: inherit;
    padding: 4px;
  }
  .hero {
    padding: 16px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .hero-num {
    font-size: 1.5rem;
    font-weight: 700;
  }
  .hero-sub {
    color: var(--muted, #666666);
    font-size: 0.9rem;
  }
  .tabs {
    display: flex;
    gap: 4px;
    padding: 0 16px;
    border-bottom: 1px solid var(--border, rgba(128, 128, 128, 0.3));
  }
  .tab {
    background: none;
    border: none;
    padding: 8px 12px;
    cursor: pointer;
    color: var(--muted, #666666);
    border-bottom: 2px solid transparent;
  }
  .tab.active {
    color: var(--fg, #1f1f1f);
    border-bottom-color: var(--accent, #3b82f6);
  }
  .grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
    padding: 16px;
    margin: 0;
  }
  .grid dt {
    color: var(--muted, #666666);
    font-size: 0.85rem;
  }
  .grid dd {
    margin: 0;
    font-size: 1.1rem;
    font-weight: 600;
  }
  .list {
    list-style: none;
    margin: 0;
    padding: 8px 16px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .list li {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 8px;
    border: 1px solid var(--border, rgba(128, 128, 128, 0.2));
    border-radius: 6px;
  }
  .kind {
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--accent, #3b82f6);
  }
  .meta {
    color: var(--muted, #666666);
    font-size: 0.8rem;
  }
  .empty,
  .notice {
    padding: 24px 16px;
    color: var(--muted, #666666);
    text-align: center;
  }
  .version-bar {
    display: flex;
    gap: 8px;
    padding: 12px 16px;
  }
  .version-bar input {
    flex: 1;
    padding: 6px 8px;
  }
</style>
