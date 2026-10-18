<!--
  Breadcrumb path bar. Click a segment to go there; click empty space,
  double-click, or press Ctrl/Cmd+L to type a path (Enter = go, Esc = cancel).
-->
<script lang="ts">
  import ChevronRight from "@lucide/svelte/icons/chevron-right";
  import HardDrive from "@lucide/svelte/icons/hard-drive";
  import House from "@lucide/svelte/icons/house";
  import { tick } from "svelte";
  import type { FileBrowser } from "../stores/files.svelte";
  import { crumbs, normalize, resolveInput } from "../util/paths";

  let { browser, editing = $bindable(false) }: { browser: FileBrowser; editing?: boolean } = $props();

  let input = $state<HTMLInputElement>();
  let scroller = $state<HTMLElement>();
  let draft = $state("");

  // A local path of "" is the drive list on Windows.
  const parts = $derived(browser.path ? crumbs(browser.path) : browser.path === "" ? [{ name: "This PC", path: "" }] : []);
  const home = $derived(browser.home ? normalize(browser.home) : null);
  /** Index of the home crumb, so everything before it can collapse to a house icon. */
  const homeIndex = $derived(home ? parts.findIndex((c) => c.path === home) : -1);
  const shown = $derived(homeIndex > 0 ? parts.slice(homeIndex) : parts);

  export async function edit() {
    draft = browser.path ?? "";
    editing = true;
    await tick();
    input?.focus();
    input?.select();
  }

  function commit() {
    const target = resolveInput(draft, browser.path ?? "/", browser.home);
    editing = false;
    if (target !== browser.path) void browser.navigate(target);
    browser.focusTick++;
  }

  function onkeydown(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === "Enter") {
      e.preventDefault();
      commit();
    } else if (e.key === "Escape") {
      e.preventDefault();
      editing = false;
      browser.focusTick++;
    }
  }

  // Keep the deepest segment visible.
  $effect(() => {
    void shown.length;
    if (scroller) scroller.scrollLeft = scroller.scrollWidth;
  });
</script>

{#if editing}
  <input
    bind:this={input}
    bind:value={draft}
    class="path-input mono"
    aria-label="Path"
    spellcheck="false"
    autocomplete="off"
    {onkeydown}
    onblur={() => (editing = false)}
  />
{:else}
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="bar" onclick={(e) => e.target === e.currentTarget && edit()} ondblclick={edit} title="Click to type a path">
    <nav class="crumbs" aria-label="Path" bind:this={scroller}>
      {#each shown as c, i (c.path)}
        {@const last = i === shown.length - 1}
        {#if homeIndex > 0 && i === 0}
          <button
            class="crumb home"
            class:last
            aria-current={last ? "location" : undefined}
            title={c.path}
            onclick={() => !last && browser.navigate(c.path)}
          >
            <House size={13} />
            {#if last}<span>{c.name}</span>{:else}<span class="sr-only">Home</span>{/if}
          </button>
        {:else}
          {#if i > 0}<span class="sep" aria-hidden="true"><ChevronRight size={12} /></span>{/if}
          <button
            class="crumb"
            class:last
            aria-current={last ? "location" : undefined}
            title={c.path}
            onclick={() => !last && browser.navigate(c.path)}
          >
            {#if i === 0 && c.name !== "/"}<HardDrive size={12} />{/if}
            <span>{c.name}</span>
          </button>
        {/if}
      {/each}
    </nav>
    <span class="fill" aria-hidden="true"></span>
  </div>
{/if}

<style>
  .bar,
  .path-input {
    flex: 1;
    min-width: 0;
    height: 28px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-input);
  }
  .bar {
    display: flex;
    align-items: center;
    padding: 0 4px;
    cursor: text;
    transition: border-color var(--dur-fast);
  }
  .bar:hover {
    border-color: var(--border-strong);
  }
  .path-input {
    padding: 0 9px;
    font-size: 12.5px;
    color: var(--text);
  }
  .path-input:focus {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .crumbs {
    display: flex;
    align-items: center;
    min-width: 0;
    overflow-x: auto;
    scrollbar-width: none;
    cursor: default;
  }
  .crumbs::-webkit-scrollbar {
    display: none;
  }
  .fill {
    flex: 1;
    align-self: stretch;
    pointer-events: none;
  }
  .crumb {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex: none;
    max-width: 220px;
    height: 22px;
    padding: 0 6px;
    border: 0;
    border-radius: var(--radius-xs);
    background: transparent;
    color: var(--text-muted);
    font-size: var(--text-base);
    white-space: nowrap;
  }
  .crumb span {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .crumb:hover:not(.last) {
    background: var(--bg-hover);
    color: var(--text);
  }
  .crumb.last {
    color: var(--text);
    font-weight: var(--weight-medium);
  }
  .crumb.home {
    color: var(--text-muted);
  }
  .sep {
    display: flex;
    flex: none;
    color: var(--text-subtle);
  }
</style>
