<script lang="ts">
  import Cable from "@lucide/svelte/icons/cable";
  import Ellipsis from "@lucide/svelte/icons/ellipsis";
  import Plug from "@lucide/svelte/icons/plug";
  import Unplug from "@lucide/svelte/icons/unplug";
  import X from "@lucide/svelte/icons/x";
  import { connectHost, disconnectHost, hostMenuItems, openHost } from "../actions";
  import { connections, STATUS_LABEL } from "../stores/connections.svelte";
  import { forwards } from "../stores/forwards.svelte";
  import { ui } from "../stores/ui.svelte";
  import type { Host } from "../types";
  import { address } from "../util/format";
  import StatusDot from "./StatusDot.svelte";

  let {
    host,
    selected,
    tabStop,
    navKey,
    highlight = [],
    onfocusrow,
  }: {
    host: Host;
    selected: boolean;
    tabStop: boolean;
    navKey: string;
    highlight?: number[];
    onfocusrow: () => void;
  } = $props();

  const conn = $derived(connections.get(host.id));
  const status = $derived(connections.status(host.id));
  const busy = $derived(status === "busy");
  const forwarding = $derived(forwards.activeFor(host.id).length);

  /** Splits the name into highlighted / plain runs for fuzzy matches. */
  const runs = $derived.by(() => {
    if (!highlight.length) return [{ text: host.name, hit: false }];
    const set = new Set(highlight);
    const out: { text: string; hit: boolean }[] = [];
    for (let i = 0; i < host.name.length; i++) {
      const hit = set.has(i);
      const last = out[out.length - 1];
      if (last && last.hit === hit) last.text += host.name[i];
      else out.push({ text: host.name[i], hit });
    }
    return out;
  });

  function menu(e: MouseEvent | KeyboardEvent, anchor?: HTMLElement) {
    e.preventDefault();
    const items = hostMenuItems(host);
    if (anchor) ui.openMenu(`${host.name} actions`, items, anchor);
    else if (e instanceof MouseEvent && e.clientX > 0) ui.openMenu(`${host.name} actions`, items, { x: e.clientX, y: e.clientY });
    else ui.openMenu(`${host.name} actions`, items, e.currentTarget as HTMLElement);
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "ContextMenu" || (e.key === "F10" && e.shiftKey)) menu(e);
  }

  function toggleConnection(e: MouseEvent) {
    e.stopPropagation();
    if (conn.phase === "connected" || busy) void disconnectHost(host);
    else connectHost(host);
  }
</script>

<li class="row" class:selected>
  <button
    type="button"
    class="main"
    data-nav={navKey}
    tabindex={tabStop ? 0 : -1}
    aria-current={selected ? "page" : undefined}
    onclick={() => openHost(host)}
    ondblclick={() => connectHost(host)}
    oncontextmenu={menu}
    onfocus={onfocusrow}
    {onkeydown}
  >
    <StatusDot {status} />
    <span class="sr-only">{STATUS_LABEL[status]}:</span>
    <span class="text">
      <span class="name">
        {#each runs as run, i (i)}{#if run.hit}<mark>{run.text}</mark>{:else}{run.text}{/if}{/each}
      </span>
      <span class="addr">{address(host)}</span>
    </span>
    {#if forwarding > 0}
      <span class="fwd" title="{forwarding} port {forwarding === 1 ? 'forward' : 'forwards'} active">
        <Cable size={12} />{#if forwarding > 1}<span>{forwarding}</span>{/if}
        <span class="sr-only">, {forwarding} port {forwarding === 1 ? "forward" : "forwards"} active</span>
      </span>
    {/if}
  </button>
  <span class="actions">
    <button
      type="button"
      class="icon-btn sm"
      tabindex="-1"
      title={conn.phase === "connected" ? "Disconnect" : busy ? "Cancel" : "Connect"}
      aria-label="{conn.phase === 'connected' ? 'Disconnect' : busy ? 'Cancel connecting to' : 'Connect to'} {host.name}"
      onclick={toggleConnection}
    >
      {#if conn.phase === "connected"}
        <Unplug size={14} />
      {:else if busy}
        <X size={14} />
      {:else}
        <Plug size={14} />
      {/if}
    </button>
    <button
      type="button"
      class="icon-btn sm"
      tabindex="-1"
      title="More actions"
      aria-label="More actions for {host.name}"
      aria-haspopup="menu"
      onclick={(e) => menu(e, e.currentTarget)}
    >
      <Ellipsis size={14} />
    </button>
  </span>
</li>

<style>
  .row {
    position: relative;
    list-style: none;
    margin: 0 8px;
  }
  .main {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    height: 42px;
    padding: 0 10px;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    text-align: left;
    color: var(--text);
    transition: background var(--dur-fast);
  }
  .main:hover {
    background: var(--bg-hover);
  }
  .main:focus-visible {
    outline-offset: -2px;
  }
  .selected .main {
    background: var(--bg-active);
  }
  .text {
    display: flex;
    flex-direction: column;
    min-width: 0;
    flex: 1;
    line-height: 1.25;
  }
  .name {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-weight: var(--weight-medium);
  }
  .addr {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--text-subtle);
    font-size: var(--text-sm);
  }
  mark {
    background: none;
    color: var(--accent-text);
    font-weight: var(--weight-semibold);
  }
  .fwd {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    flex: none;
    color: var(--success-text);
    font-size: var(--text-xs);
    font-weight: var(--weight-medium);
    font-variant-numeric: tabular-nums;
    opacity: 0.85;
  }
  .row:hover .fwd,
  .row:focus-within .fwd {
    display: none;
  }
  .actions {
    position: absolute;
    top: 50%;
    right: 6px;
    display: none;
    gap: 2px;
    transform: translateY(-50%);
  }
  .row:hover .actions,
  .row:focus-within .actions {
    display: flex;
  }
  .row:hover .text,
  .row:focus-within .text {
    padding-right: 48px;
  }
</style>
