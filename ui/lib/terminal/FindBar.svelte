<!-- Find-in-terminal bar, floating at the top right of a pane. -->
<script lang="ts">
  import CaseSensitive from "@lucide/svelte/icons/case-sensitive";
  import ChevronDown from "@lucide/svelte/icons/chevron-down";
  import ChevronUp from "@lucide/svelte/icons/chevron-up";
  import Regex from "@lucide/svelte/icons/regex";
  import Search from "@lucide/svelte/icons/search";
  import X from "@lucide/svelte/icons/x";
  import { tick } from "svelte";
  import type { TermSession } from "../stores/terminals.svelte";
  import { passesToApp } from "./keys";

  let { session }: { session: TermSession } = $props();

  let input = $state<HTMLInputElement>();
  let query = $state("");
  let caseSensitive = $state(false);
  let regex = $state(false);

  // (Re)focus when opened or when the shortcut is pressed again.
  $effect(() => {
    void session.findTick;
    void tick().then(() => {
      input?.focus();
      input?.select();
    });
  });

  const opts = () => ({ caseSensitive, regex });

  function run(dir: "next" | "prev", incremental = false) {
    session.find(query, dir, { ...opts(), incremental });
  }

  function oninput() {
    run("next", true);
  }

  function toggle(which: "case" | "regex") {
    if (which === "case") caseSensitive = !caseSensitive;
    else regex = !regex;
    run("next", true);
    input?.focus();
  }

  function onkeydown(e: KeyboardEvent) {
    // Keep keys away from the terminal view and most global shortcuts.
    if (!passesToApp(e)) e.stopPropagation();
    if (e.key === "Enter") {
      e.preventDefault();
      run(e.shiftKey ? "prev" : "next");
    } else if (e.key === "Escape") {
      e.preventDefault();
      session.closeFind();
    } else if (e.key === "F3") {
      e.preventDefault();
      run(e.shiftKey ? "prev" : "next");
    }
  }

  const status = $derived.by(() => {
    const r = session.searchResult;
    if (!query || !r) return "";
    if (r.count === 0) return "No results";
    if (r.index < 0) return `${r.count.toLocaleString()}${r.count >= 2000 ? "+" : ""} found`;
    return `${(r.index + 1).toLocaleString()} of ${r.count.toLocaleString()}${r.count >= 2000 ? "+" : ""}`;
  });
  const none = $derived(!!query && session.searchResult?.count === 0);
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="find" role="search" onpointerdown={(e) => e.stopPropagation()}>
  <Search size={13} class="find-icon" />
  <input
    bind:this={input}
    bind:value={query}
    class:none
    placeholder="Find"
    aria-label="Find in terminal"
    spellcheck="false"
    autocomplete="off"
    {oninput}
    {onkeydown}
  />
  <span class="count" aria-live="polite">{status}</span>
  <button
    class="icon-btn sm"
    class:on={caseSensitive}
    aria-pressed={caseSensitive}
    aria-label="Match case"
    title="Match case"
    onclick={() => toggle("case")}
  >
    <CaseSensitive size={15} />
  </button>
  <button class="icon-btn sm" class:on={regex} aria-pressed={regex} aria-label="Regular expression" title="Regular expression" onclick={() => toggle("regex")}>
    <Regex size={13} />
  </button>
  <span class="div" aria-hidden="true"></span>
  <button class="icon-btn sm" aria-label="Previous match" title="Previous (Shift+Enter)" disabled={!query} onclick={() => run("prev")}>
    <ChevronUp size={15} />
  </button>
  <button class="icon-btn sm" aria-label="Next match" title="Next (Enter)" disabled={!query} onclick={() => run("next")}>
    <ChevronDown size={15} />
  </button>
  <button class="icon-btn sm" aria-label="Close find" title="Close (Esc)" onclick={() => session.closeFind()}>
    <X size={14} />
  </button>
</div>

<style>
  .find {
    position: absolute;
    top: 8px;
    right: 18px;
    z-index: 6;
    display: flex;
    align-items: center;
    gap: 2px;
    width: min(380px, calc(100% - 28px));
    height: 34px;
    padding: 0 4px 0 9px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    box-shadow: var(--shadow-lg);
    animation: drop var(--dur-fast) var(--ease-out);
  }
  .find :global(.find-icon) {
    flex: none;
    color: var(--text-subtle);
  }
  input {
    flex: 1;
    min-width: 40px;
    height: 100%;
    padding: 0 6px;
    border: 0;
    background: transparent;
    font-size: var(--text-base);
  }
  input:focus,
  input:focus-visible {
    outline: none;
  }
  input::placeholder {
    color: var(--text-subtle);
  }
  input.none {
    color: var(--danger-text);
  }
  .count {
    flex: none;
    padding: 0 4px;
    color: var(--text-muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .icon-btn.on {
    background: var(--accent-soft);
    color: var(--accent-text);
  }
  .div {
    width: 1px;
    height: 16px;
    margin: 0 3px;
    background: var(--border);
    flex: none;
  }
  @container (max-width: 300px) {
    .count {
      display: none;
    }
  }
  @keyframes drop {
    from {
      opacity: 0;
      transform: translateY(-4px);
    }
  }
</style>
