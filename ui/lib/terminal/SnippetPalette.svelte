<!--
  Snippet picker for a terminal tab: fuzzy filter over the global snippets and
  the ones scoped to this host. Enter inserts into the focused pane (and runs
  it if the snippet says so); Shift+Enter only inserts.
-->
<script lang="ts">
  import CornerDownLeft from "@lucide/svelte/icons/corner-down-left";
  import Play from "@lucide/svelte/icons/play";
  import Plus from "@lucide/svelte/icons/plus";
  import Search from "@lucide/svelte/icons/search";
  import Settings2 from "@lucide/svelte/icons/settings-2";
  import SquareCode from "@lucide/svelte/icons/square-code";
  import { onMount, tick } from "svelte";
  import { hosts } from "../stores/hosts.svelte";
  import { snippets } from "../stores/snippets.svelte";
  import type { TermSession } from "../stores/terminals.svelte";
  import { ui } from "../stores/ui.svelte";
  import type { HostId, Snippet } from "../types";
  import { matchField } from "../util/fuzzy";

  let {
    hostId,
    session,
    onclose,
  }: {
    hostId: HostId | null;
    /** Pane that receives the snippet. */
    session: TermSession | undefined;
    onclose: (refocus: boolean) => void;
  } = $props();

  let el = $state<HTMLDivElement>();
  let input = $state<HTMLInputElement>();
  let listEl = $state<HTMLElement>();
  let query = $state("");
  let index = $state(0);

  onMount(() => {
    if (!snippets.loaded) void snippets.load();
    void tick().then(() => input?.focus());
  });

  interface Result {
    snippet: Snippet;
    score: number;
    hits: Set<number>;
  }

  const available = $derived(snippets.forHost(hostId));
  const results = $derived.by((): Result[] => {
    const tokens = query.toLowerCase().split(/\s+/).filter(Boolean);
    const out: Result[] = [];
    for (const snippet of available) {
      let score = 0;
      const hits = new Set<number>();
      let ok = true;
      for (const t of tokens) {
        const name = matchField(t, snippet.name);
        const cmd = matchField(t, snippet.command);
        if (!name && !cmd) {
          ok = false;
          break;
        }
        if (name && (!cmd || name.score >= cmd.score * 0.7)) {
          score += name.score;
          name.indices.forEach((i) => hits.add(i));
        } else if (cmd) score += cmd.score * 0.7;
      }
      if (ok) out.push({ snippet, score, hits });
    }
    return tokens.length ? out.sort((a, b) => b.score - a.score) : out;
  });

  $effect(() => {
    void query;
    index = 0;
  });

  $effect(() => {
    const i = index;
    void tick().then(() => listEl?.querySelector(`[data-index="${i}"]`)?.scrollIntoView({ block: "nearest" }));
  });

  const canInsert = $derived(session?.status === "running");

  function choose(r: Result | undefined, insertOnly = false) {
    if (!r || !session || !canInsert) return;
    onclose(false);
    session.insert(r.snippet.command, r.snippet.run && !insertOnly);
  }

  function onkeydown(e: KeyboardEvent) {
    e.stopPropagation();
    const n = results.length;
    switch (e.key) {
      case "ArrowDown":
        if (n) index = (index + 1) % n;
        break;
      case "ArrowUp":
        if (n) index = (index - 1 + n) % n;
        break;
      case "PageDown":
        index = Math.min(n - 1, index + 6);
        break;
      case "PageUp":
        index = Math.max(0, index - 6);
        break;
      case "Enter":
        choose(results[index], e.shiftKey);
        break;
      case "Escape":
        onclose(true);
        break;
      case "Tab":
        // Focus stays in the palette (combobox pattern).
        break;
      default:
        return;
    }
    e.preventDefault();
  }

  function manage(select?: string) {
    onclose(false);
    ui.snippets = { select: select ?? (results[index]?.snippet.id || undefined), hostId };
  }

  function createNew() {
    onclose(false);
    ui.snippets = { select: "new", hostId };
  }

  function onwindowpointer(e: PointerEvent) {
    if (el && !el.contains(e.target as Node) && !(e.target as Element).closest?.(".menu")) onclose(false);
  }

  function firstLine(cmd: string): { text: string; more: number } {
    const lines = cmd.replace(/\n+$/, "").split("\n");
    return { text: lines[0], more: lines.length - 1 };
  }

  function scopeLabel(s: Snippet): string | null {
    if (!s.hostIds.length) return null;
    if (hostId && s.hostIds.includes(hostId)) return hosts.get(hostId)?.name ?? "This host";
    return null;
  }

  function runs(name: string, hits: Set<number>): { text: string; hit: boolean }[] {
    if (!hits.size) return [{ text: name, hit: false }];
    const out: { text: string; hit: boolean }[] = [];
    for (let i = 0; i < name.length; i++) {
      const hit = hits.has(i);
      const last = out[out.length - 1];
      if (last && last.hit === hit) last.text += name[i];
      else out.push({ text: name[i], hit });
    }
    return out;
  }
</script>

<svelte:window onpointerdown={onwindowpointer} />

<div class="palette" bind:this={el} role="dialog" aria-label="Insert snippet" tabindex="-1">
  <div class="search">
    <Search size={15} class="s-icon" />
    <input
      bind:this={input}
      bind:value={query}
      placeholder="Search snippets"
      role="combobox"
      aria-expanded="true"
      aria-controls="snippet-list"
      aria-activedescendant={results[index] ? `snippet-opt-${results[index].snippet.id}` : undefined}
      aria-autocomplete="list"
      spellcheck="false"
      autocomplete="off"
      {onkeydown}
    />
    <kbd aria-hidden="true">Esc</kbd>
  </div>

  {#if !snippets.loaded}
    <div class="empty"><span class="muted">Loading…</span></div>
  {:else if available.length === 0}
    <div class="empty">
      <span class="empty-icon"><SquareCode size={18} /></span>
      <div class="empty-title">No snippets yet</div>
      <p class="muted">Save commands you run often and insert them with a keystroke.</p>
      <button class="btn btn-sm btn-primary" onclick={createNew}><Plus size={13} /> New snippet</button>
    </div>
  {:else if results.length === 0}
    <div class="empty">
      <div class="empty-title">No matches</div>
      <p class="muted">No snippet matches “{query.trim()}”.</p>
    </div>
  {:else}
    <ul class="list" id="snippet-list" role="listbox" aria-label="Snippets" bind:this={listEl}>
      {#each results as r, i (r.snippet.id)}
        {@const line = firstLine(r.snippet.command)}
        {@const scope = scopeLabel(r.snippet)}
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <li
          id="snippet-opt-{r.snippet.id}"
          role="option"
          aria-selected={i === index}
          class:active={i === index}
          data-index={i}
          onclick={(e) => choose(r, e.shiftKey)}
          onpointermove={() => (index = i)}
        >
          <div class="top">
            <span class="name">
              {#each runs(r.snippet.name, r.hits) as run, k (k)}{#if run.hit}<mark>{run.text}</mark>{:else}{run.text}{/if}{/each}
            </span>
            {#if scope}<span class="badge scope" title="Only for {scope}">{scope}</span>{/if}
            <span class="spacer"></span>
            {#if r.snippet.run}
              <span class="mode" title="Runs immediately"><Play size={11} /> Runs</span>
            {:else}
              <span class="mode" title="Inserted at the prompt without running">Inserts</span>
            {/if}
          </div>
          <div class="cmd mono">
            <span class="cmd-text">{line.text}</span>
            {#if line.more}<span class="more">+{line.more} {line.more === 1 ? "line" : "lines"}</span>{/if}
          </div>
        </li>
      {/each}
    </ul>
  {/if}

  <footer class="foot">
    <button class="btn btn-ghost btn-sm" onclick={() => manage()}><Settings2 size={13} /> Manage snippets…</button>
    <span class="spacer"></span>
    {#if !canInsert}
      <span class="hint warn">The shell isn’t running</span>
    {:else}
      <span class="hint"><kbd><CornerDownLeft size={10} /></kbd> insert</span>
      <span class="hint"><kbd>⇧</kbd><kbd><CornerDownLeft size={10} /></kbd> don’t run</span>
    {/if}
  </footer>
</div>

<style>
  .palette {
    position: absolute;
    top: 10px;
    left: 50%;
    z-index: 20;
    display: flex;
    flex-direction: column;
    width: min(540px, calc(100% - 24px));
    max-height: min(420px, calc(100% - 20px));
    transform: translateX(-50%);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-elevated);
    box-shadow: var(--shadow-lg);
    overflow: hidden;
    animation: pop var(--dur-fast) var(--ease-out);
  }
  .palette:focus {
    outline: none;
  }
  .search {
    position: relative;
    display: flex;
    align-items: center;
    gap: 8px;
    flex: none;
    height: 44px;
    padding: 0 12px;
    border-bottom: 1px solid var(--border-subtle);
  }
  .search :global(.s-icon) {
    flex: none;
    color: var(--text-subtle);
  }
  .search input {
    flex: 1;
    min-width: 0;
    height: 100%;
    border: 0;
    background: transparent;
    font-size: var(--text-md);
  }
  .search input:focus,
  .search input:focus-visible {
    outline: none;
  }
  .search input::placeholder {
    color: var(--text-subtle);
  }
  .list {
    flex: 1;
    min-height: 0;
    margin: 0;
    padding: 4px;
    overflow-y: auto;
    list-style: none;
  }
  li {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 7px 10px 8px;
    border-radius: var(--radius-sm);
  }
  li.active {
    background: var(--bg-active);
  }
  .top {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .name {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-weight: var(--weight-medium);
  }
  mark {
    background: none;
    color: var(--accent-text);
    font-weight: var(--weight-semibold);
  }
  .scope {
    max-width: 140px;
    overflow: hidden;
    text-overflow: ellipsis;
    height: 18px;
    font-size: 10.5px;
  }
  .spacer {
    flex: 1;
  }
  .mode {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    flex: none;
    color: var(--text-subtle);
    font-size: var(--text-xs);
  }
  li.active .mode {
    color: var(--text-muted);
  }
  .cmd {
    display: flex;
    gap: 8px;
    min-width: 0;
    color: var(--text-muted);
    font-size: 12px;
  }
  .cmd-text {
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .more {
    flex: none;
    color: var(--text-subtle);
    font-family: var(--font-sans);
    font-size: var(--text-xs);
  }
  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 4px;
    padding: 28px 24px;
    text-align: center;
  }
  .empty .btn {
    margin-top: 10px;
  }
  .empty-icon {
    display: grid;
    place-items: center;
    width: 38px;
    height: 38px;
    margin-bottom: 6px;
    border-radius: var(--radius-md);
    background: var(--accent-soft);
    color: var(--accent-text);
  }
  .empty-title {
    font-weight: var(--weight-semibold);
  }
  .muted {
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .foot {
    display: flex;
    align-items: center;
    gap: 10px;
    flex: none;
    height: 38px;
    padding: 0 10px 0 6px;
    border-top: 1px solid var(--border-subtle);
    background: var(--bg-subtle);
  }
  .hint {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    color: var(--text-subtle);
    font-size: var(--text-xs);
    white-space: nowrap;
  }
  .hint.warn {
    color: var(--warning-text);
  }
  @container (max-width: 420px) {
    .hint:not(.warn) + .hint {
      display: none;
    }
  }
  @keyframes pop {
    from {
      opacity: 0;
      transform: translateX(-50%) translateY(-4px) scale(0.98);
    }
  }
</style>
