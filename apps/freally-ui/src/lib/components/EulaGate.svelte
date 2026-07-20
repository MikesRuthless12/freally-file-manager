<!--
  First-run EULA acceptance gate. Rendered by App instead of the main
  UI until the current EULA version is accepted. The user must scroll
  to the end before "I Agree" enables; "Decline & Quit" exits the app.
  Acceptance is persisted, so the gate appears once (and again only
  when the embedded EULA version changes).
-->
<script lang="ts">
  import { onMount } from "svelte";

  import { t } from "../i18n";
  import { eulaAccept, eulaDeclineQuit } from "../ipc";
  import type { EulaStatus } from "../types";

  type Props = {
    status: EulaStatus;
    onAccepted: () => void;
  };

  let { status, onAccepted }: Props = $props();

  let scrolledToEnd = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let scrollEl: HTMLDivElement | null = $state(null);

  // If the agreement is short enough not to scroll, enable Agree
  // immediately. Measured after paint so scrollHeight is real.
  onMount(() => {
    const id = requestAnimationFrame(() => {
      if (scrollEl && scrollEl.scrollHeight <= scrollEl.clientHeight + 4) {
        scrolledToEnd = true;
      }
    });
    return () => cancelAnimationFrame(id);
  });

  function onScroll() {
    if (
      scrollEl &&
      scrollEl.scrollTop + scrollEl.clientHeight >= scrollEl.scrollHeight - 24
    ) {
      scrolledToEnd = true;
    }
  }

  async function agree() {
    if (busy || !scrolledToEnd) return;
    busy = true;
    error = null;
    try {
      await eulaAccept();
      onAccepted();
    } catch (e) {
      busy = false;
      error = String(e);
    }
  }

  function decline() {
    void eulaDeclineQuit();
  }

  // ------------------------------------------------------------------
  // Tiny markdown → block renderer for the embedded EULA (headings,
  // bold, code, lists, blockquotes, paragraphs). No external parser:
  // the text is build-time-embedded and trusted, and Svelte's template
  // escaping means there is no injection surface.
  // ------------------------------------------------------------------

  type Inline = { kind: "text" | "strong" | "em" | "code"; text: string };
  type Block =
    | { kind: "h1" | "h2" | "p" | "quote"; parts: Inline[] }
    | { kind: "ul"; items: Inline[][] };

  function inline(text: string): Inline[] {
    // `**bold**` is matched before `*italic*` so a bold pair is never
    // mis-split into two emphasis runs.
    return text
      .split(/(\*\*[^*]+\*\*|\*[^*]+\*|`[^`]+`)/g)
      .filter((part) => part.length > 0)
      .map((part): Inline => {
        if (part.startsWith("**") && part.endsWith("**")) {
          return { kind: "strong", text: part.slice(2, -2) };
        }
        if (part.startsWith("`") && part.endsWith("`")) {
          return { kind: "code", text: part.slice(1, -1) };
        }
        if (part.length > 2 && part.startsWith("*") && part.endsWith("*")) {
          return { kind: "em", text: part.slice(1, -1) };
        }
        return { kind: "text", text: part };
      });
  }

  function parseMarkdown(text: string): Block[] {
    const blocks: Block[] = [];
    let list: Inline[][] = [];
    // Consecutive non-blank body lines form one paragraph — the source
    // is hard-wrapped, and inline spans (bold/italic) can straddle the
    // wrap, so we join before inline-parsing rather than emit one <p>
    // per source line.
    let para: string[] = [];
    const flushList = () => {
      if (list.length === 0) return;
      blocks.push({ kind: "ul", items: list });
      list = [];
    };
    const flushPara = () => {
      if (para.length === 0) return;
      blocks.push({ kind: "p", parts: inline(para.join(" ")) });
      para = [];
    };
    const flush = () => {
      flushPara();
      flushList();
    };
    for (const line of text.split("\n")) {
      if (/^#{2,}\s/.test(line)) {
        flush();
        blocks.push({ kind: "h2", parts: inline(line.replace(/^#{2,}\s/, "")) });
      } else if (/^#\s/.test(line)) {
        flush();
        blocks.push({ kind: "h1", parts: inline(line.replace(/^#\s/, "")) });
      } else if (/^>\s?/.test(line)) {
        flush();
        const body = line.replace(/^>\s?/, "");
        if (body.trim() !== "") {
          blocks.push({ kind: "quote", parts: inline(body) });
        }
      } else if (/^[-*]\s/.test(line)) {
        flushPara();
        list.push(inline(line.replace(/^[-*]\s/, "")));
      } else if (line.trim() === "") {
        flush();
      } else {
        flushList();
        para.push(line);
      }
    }
    flush();
    return blocks;
  }

  const blocks = $derived(parseMarkdown(status.text));
</script>

{#snippet inlineParts(parts: Inline[])}
  {#each parts as part, i (i)}
    {#if part.kind === "strong"}<strong>{part.text}</strong
      >{:else if part.kind === "em"}<em>{part.text}</em
      >{:else if part.kind === "code"}<code>{part.text}</code
      >{:else}{part.text}{/if}
  {/each}
{/snippet}

<div class="gate" role="dialog" aria-modal="true" aria-labelledby="eula-title">
  <div class="panel">
    <div class="head">
      <h1 id="eula-title">{t("eula-title")}</h1>
      <span class="version">{t("eula-version", { version: status.version })}</span>
    </div>
    <p class="intro">{t("eula-intro")}</p>
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -- the agreement
         text must be keyboard-scrollable (arrow/page keys) to reach the
         end, which is what enables "I Agree". -->
    <div
      class="text"
      role="region"
      aria-label={t("eula-title")}
      bind:this={scrollEl}
      onscroll={onScroll}
      tabindex="0"
    >
      {#each blocks as block, i (i)}
        {#if block.kind === "h1"}
          <h2>{@render inlineParts(block.parts)}</h2>
        {:else if block.kind === "h2"}
          <h3>{@render inlineParts(block.parts)}</h3>
        {:else if block.kind === "quote"}
          <p class="quote">{@render inlineParts(block.parts)}</p>
        {:else if block.kind === "ul"}
          <ul>
            {#each block.items as item, j (j)}
              <li>{@render inlineParts(item)}</li>
            {/each}
          </ul>
        {:else}
          <p>{@render inlineParts(block.parts)}</p>
        {/if}
      {/each}
    </div>
    {#if error}
      <p class="error" role="alert">{t("eula-error", { error })}</p>
    {/if}
    <div class="foot">
      <span class="hint" aria-live="polite">
        {scrolledToEnd ? t("eula-thanks") : t("eula-scroll-hint")}
      </span>
      <div class="actions">
        <button type="button" class="decline" onclick={decline}>
          {t("eula-decline")}
        </button>
        <button
          type="button"
          class="agree"
          onclick={agree}
          disabled={!scrolledToEnd || busy}
        >
          {t("eula-agree")}
        </button>
      </div>
    </div>
  </div>
</div>

<style>
  .gate {
    position: fixed;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--bg, #fafafa);
    z-index: 1000;
    padding: 16px;
  }
  .panel {
    display: flex;
    flex-direction: column;
    gap: 10px;
    width: 100%;
    max-width: 680px;
    max-height: 100%;
    background: var(--surface, #ffffff);
    border: 1px solid var(--border, rgba(0, 0, 0, 0.1));
    border-radius: 10px;
    padding: 18px;
    box-shadow: 0 16px 48px rgba(0, 0, 0, 0.25);
  }
  .head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
  }
  h1 {
    margin: 0;
    font-size: 16px;
    color: var(--fg-strong, #1f1f1f);
  }
  .version {
    flex-shrink: 0;
    font-family: monospace;
    font-size: 10px;
    color: var(--fg-dim, #6a6a6a);
  }
  .intro {
    margin: 0;
    font-size: 12px;
    color: var(--fg-dim, #6a6a6a);
  }
  .text {
    flex: 1;
    min-height: 0;
    overflow: auto;
    border: 1px solid var(--border, rgba(0, 0, 0, 0.1));
    border-radius: 6px;
    padding: 10px 12px;
    font-size: 11px;
    line-height: 1.5;
    color: var(--fg, #1f1f1f);
    background: var(--bg, #fafafa);
  }
  .text h2 {
    margin: 8px 0 6px;
    font-size: 13px;
    color: var(--fg-strong, #1f1f1f);
  }
  .text h3 {
    margin: 10px 0 4px;
    font-size: 12px;
    color: var(--fg-strong, #1f1f1f);
  }
  .text p {
    margin: 4px 0;
  }
  .text .quote {
    border-inline-start: 2px solid var(--accent, #4f8cff);
    padding-inline-start: 8px;
    font-style: italic;
  }
  .text ul {
    margin: 4px 0;
    padding-inline-start: 18px;
  }
  .text code {
    font-family: monospace;
    font-size: 10px;
    background: var(--hover, rgba(0, 0, 0, 0.04));
    border-radius: 3px;
    padding: 0 3px;
  }
  .error {
    margin: 0;
    font-size: 11px;
    color: var(--error, #d95757);
  }
  .foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: 8px;
  }
  .hint {
    font-size: 10px;
    color: var(--fg-dim, #6a6a6a);
  }
  .actions {
    display: flex;
    gap: 8px;
  }
  .actions button {
    padding: 6px 14px;
    border-radius: 6px;
    font-size: 12px;
    cursor: pointer;
  }
  .decline {
    background: transparent;
    color: var(--fg, #1f1f1f);
    border: 1px solid var(--border, rgba(0, 0, 0, 0.1));
  }
  .decline:hover {
    border-color: var(--error, #d95757);
    color: var(--error, #d95757);
  }
  .agree {
    background: var(--accent, #4f8cff);
    color: #ffffff;
    border: 1px solid transparent;
    font-weight: 600;
  }
  .agree:disabled {
    opacity: 0.4;
    cursor: default;
  }
</style>
