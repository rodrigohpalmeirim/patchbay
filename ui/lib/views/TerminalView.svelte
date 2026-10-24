<!--
  Terminal tab: toolbar + pane area. Panes are rendered flat (keyed by pane
  id) and positioned from the pane tree, so splitting or closing panes never
  remounts a terminal. Sessions live in stores/terminals.svelte.ts and are
  disposed when their pane or this tab is closed.
-->
<script lang="ts">
  import Columns2 from "@lucide/svelte/icons/columns-2";
  import Ellipsis from "@lucide/svelte/icons/ellipsis";
  import Laptop from "@lucide/svelte/icons/laptop";
  import Maximize2 from "@lucide/svelte/icons/maximize-2";
  import Minimize2 from "@lucide/svelte/icons/minimize-2";
  import Rows2 from "@lucide/svelte/icons/rows-2";
  import Search from "@lucide/svelte/icons/search";
  import Server from "@lucide/svelte/icons/server";
  import SquareCode from "@lucide/svelte/icons/square-code";
  import { onDestroy, untrack } from "svelte";
  import OsIcon from "../components/OsIcon.svelte";
  import StatusDot from "../components/StatusDot.svelte";
  import { connections } from "../stores/connections.svelte";
  import { tabs, type TerminalTab } from "../stores/tabs.svelte";
  import { terminals, type SessionHooks } from "../stores/terminals.svelte";
  import type { Host } from "../types";
  import { keyLabel, terminalAction } from "../terminal/keys";
  import { clampRatio, computeLayout, findLeaf, findSplit, leaves, type Divider } from "../terminal/layout";
  import { displayPath, openSnippets, runAction, showPaneMenu, toggleZoom } from "../terminal/ops";
  import SnippetPalette from "../terminal/SnippetPalette.svelte";
  import TerminalPane from "../terminal/TerminalPane.svelte";
  import { isEditable } from "../util/platform";

  let { tab, host, active }: { tab: TerminalTab; host: Host | null; active: boolean } = $props();

  let area = $state<HTMLDivElement>();
  let width = $state(800);

  const paneList = $derived(leaves(tab.layout));
  const layout = $derived(computeLayout(tab.layout));
  const multi = $derived(paneList.length > 1 && !tab.zoomedPane);
  const focused = $derived(terminals.get(tab.focusedPane));
  const conn = $derived(host ? connections.get(host.id) : null);

  function hooksFor(paneId: string): SessionHooks {
    return {
      onAction: (action) => runAction(tab, paneId, action),
      onFocus: () => {
        if (tab.focusedPane !== paneId) tab.focusedPane = paneId;
        tabs.focusGroup(tabs.groupOf(tab.id));
      },
      onCwd: (cwd) => {
        const leaf = findLeaf(tab.layout, paneId);
        if (leaf && leaf.cwd !== cwd) leaf.cwd = cwd;
      },
    };
  }

  // Create sessions for new panes; drop sessions whose pane vanished. A
  // restored tab starts its shells only once it's first shown (dormant).
  const dormant = $derived(tabs.isDormant(tab.id));
  $effect.pre(() => {
    const ids = new Set(paneList.map((l) => l.id));
    if (dormant) return;
    untrack(() => {
      for (const leaf of paneList) terminals.ensure(leaf.id, tab.id, tab.hostId, leaf.cwd, hooksFor(leaf.id));
      for (const [id, s] of terminals.sessions) if (s.tabId === tab.id && !ids.has(id)) terminals.dispose(id);
    });
  });

  onDestroy(() => {
    terminals.disposeTab(tab.id);
    if (terminals.paletteFor === tab.id) terminals.paletteFor = null;
  });

  // Keyboard focus: requested by actions (open, split, close…) or when the
  // tab becomes visible while nothing else has focus (Ctrl+Tab etc.).
  $effect(() => {
    if (terminals.focusRequest !== tab.id || !active) return;
    const s = terminals.get(tab.focusedPane);
    if (!s || s.status === "loading") return;
    untrack(() => {
      terminals.focusRequest = null;
      requestAnimationFrame(() => s.focus());
    });
  });
  $effect(() => {
    if (!active) return;
    untrack(() => {
      // Focus left behind in a now-hidden panel (Ctrl+Tab from another terminal) counts as nothing.
      const ae = document.activeElement;
      if (!ae || ae === document.body || ae.closest("[hidden]")) terminals.requestFocus(tab.id);
    });
  });

  // ------------------------------------------------------------ dividers

  let dragging = $state<string | null>(null);
  let dragDir = $state<"row" | "column">("row");

  function dividerStyle(d: Divider): string {
    const a = d.area;
    if (d.dir === "row") {
      return `left:${(a.x + a.w * d.ratio) * 100}%;top:${a.y * 100}%;height:${a.h * 100}%`;
    }
    return `top:${(a.y + a.h * d.ratio) * 100}%;left:${a.x * 100}%;width:${a.w * 100}%`;
  }

  const MIN_PX = 80;

  function setRatio(d: Divider, ratio: number) {
    const split = findSplit(tab.layout, d.splitId);
    if (!split || !area) return;
    const total = d.dir === "row" ? area.clientWidth * d.area.w : area.clientHeight * d.area.h;
    const min = total > 0 ? Math.min(0.45, MIN_PX / total) : 0.1;
    split.ratio = Math.min(1 - min, Math.max(min, clampRatio(ratio)));
  }

  function ondividerdown(e: PointerEvent, d: Divider) {
    if (!area || e.button !== 0) return;
    e.preventDefault();
    const rect = area.getBoundingClientRect();
    const target = e.currentTarget as HTMLElement;
    target.setPointerCapture(e.pointerId);
    dragging = d.splitId;
    dragDir = d.dir;
    const move = (ev: PointerEvent) => {
      const r =
        d.dir === "row"
          ? (ev.clientX - rect.left - d.area.x * rect.width) / (d.area.w * rect.width)
          : (ev.clientY - rect.top - d.area.y * rect.height) / (d.area.h * rect.height);
      setRatio(d, r);
    };
    const up = () => {
      dragging = null;
      target.removeEventListener("pointermove", move);
      target.removeEventListener("pointerup", up);
      target.removeEventListener("pointercancel", up);
    };
    target.addEventListener("pointermove", move);
    target.addEventListener("pointerup", up);
    target.addEventListener("pointercancel", up);
  }

  function ondividerkey(e: KeyboardEvent, d: Divider) {
    const step = 0.03;
    const back = d.dir === "row" ? "ArrowLeft" : "ArrowUp";
    const fwd = d.dir === "row" ? "ArrowRight" : "ArrowDown";
    if (e.key === back) setRatio(d, d.ratio - step);
    else if (e.key === fwd) setRatio(d, d.ratio + step);
    else if (e.key === "Home") setRatio(d, 0.5);
    else return;
    e.preventDefault();
    e.stopPropagation();
  }

  // ------------------------------------------------------------ keyboard

  /** Shortcuts while focus is on the toolbar etc. (xterm handles its own). */
  function onkeydown(e: KeyboardEvent) {
    if (e.defaultPrevented || isEditable(e.target)) return;
    const action = terminalAction(e);
    if (!action || e.type !== "keydown") return;
    e.preventDefault();
    e.stopPropagation();
    runAction(tab, tab.focusedPane, action);
  }

  function moreMenu(e: MouseEvent) {
    showPaneMenu(tab, tab.focusedPane, e.currentTarget as HTMLElement);
  }

  function closePalette(refocus: boolean) {
    if (terminals.paletteFor === tab.id) terminals.paletteFor = null;
    if (refocus) terminals.get(tab.focusedPane)?.focus();
  }

  const cwdLabel = $derived(focused?.cwd ? displayPath(focused.cwd, tab.hostId) : null);
  const compact = $derived(width < 520);
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="terminal-view" bind:clientWidth={width} {onkeydown}>
  <div class="toolbar">
    <div class="where" title={host ? `${host.name} — ${host.hostname}` : "This computer"}>
      <span class="where-icon">
        {#if !host}
          <Laptop size={14} />
        {:else if conn?.capabilities && conn.capabilities.os !== "unknown"}
          <OsIcon os={conn.capabilities.os} size={14} />
        {:else}
          <Server size={14} />
        {/if}
        {#if host}<span class="dot"><StatusDot status={connections.status(host.id)} size={6} /></span>{/if}
      </span>
      <span class="where-name">{host ? host.name : "Local"}</span>
      {#if cwdLabel && !compact}
        <span class="where-sep" aria-hidden="true">/</span>
        <span class="where-cwd mono" title={focused?.cwd ?? undefined}>{cwdLabel}</span>
      {/if}
      {#if paneList.length > 1}
        <span class="badge panes" title="{paneList.length} panes">{paneList.length} panes</span>
      {/if}
    </div>
    <span class="spacer"></span>
    <div class="group">
      <button
        class="icon-btn"
        class:on={terminals.paletteFor === tab.id}
        aria-label="Snippets"
        title="Snippets ({keyLabel('snippets')})"
        onclick={() => (terminals.paletteFor === tab.id ? closePalette(true) : openSnippets(tab))}
      >
        <SquareCode size={15} />
      </button>
      <button class="icon-btn" aria-label="Find" title="Find ({keyLabel('find')})" onclick={() => runAction(tab, tab.focusedPane, "find")}>
        <Search size={15} />
      </button>
      {#if !compact}
        <span class="sep" aria-hidden="true"></span>
        <button class="icon-btn" aria-label="Split right" title="Split right ({keyLabel('splitRight')})" onclick={() => runAction(tab, tab.focusedPane, "splitRight")}>
          <Columns2 size={15} />
        </button>
        <button class="icon-btn" aria-label="Split down" title="Split down ({keyLabel('splitDown')})" onclick={() => runAction(tab, tab.focusedPane, "splitDown")}>
          <Rows2 size={15} />
        </button>
        {#if paneList.length > 1}
          <button
            class="icon-btn"
            class:on={!!tab.zoomedPane}
            aria-pressed={!!tab.zoomedPane}
            aria-label={tab.zoomedPane ? "Restore pane size" : "Maximize pane"}
            title="{tab.zoomedPane ? 'Restore pane size' : 'Maximize pane'} ({keyLabel('zoom')})"
            onclick={() => toggleZoom(tab)}
          >
            {#if tab.zoomedPane}<Minimize2 size={15} />{:else}<Maximize2 size={15} />{/if}
          </button>
        {/if}
      {/if}
      <button class="icon-btn" aria-label="Terminal actions" aria-haspopup="menu" title="More" onclick={moreMenu}>
        <Ellipsis size={15} />
      </button>
    </div>
  </div>

  <div class="area" class:dragging={!!dragging} class:rows={dragDir === "column"} bind:this={area}>
    {#each paneList as leaf (leaf.id)}
      {@const session = terminals.get(leaf.id)}
      {@const zoomed = tab.zoomedPane === leaf.id}
      {@const rect = zoomed ? { x: 0, y: 0, w: 1, h: 1 } : (layout.rects.get(leaf.id) ?? { x: 0, y: 0, w: 1, h: 1 })}
      {#if session}
        <TerminalPane
          {tab}
          {session}
          {host}
          {rect}
          focused={tab.focusedPane === leaf.id}
          {multi}
          hidden={!!tab.zoomedPane && !zoomed}
        />
      {/if}
    {/each}

    {#if !tab.zoomedPane}
      {#each layout.dividers as d (d.splitId)}
        <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
        <div
          class="divider {d.dir}"
          class:active={dragging === d.splitId}
          style={dividerStyle(d)}
          role="separator"
          aria-orientation={d.dir === "row" ? "vertical" : "horizontal"}
          aria-label="Resize panes"
          aria-valuenow={Math.round(d.ratio * 100)}
          aria-valuemin={10}
          aria-valuemax={90}
          tabindex="0"
          onpointerdown={(e) => ondividerdown(e, d)}
          onkeydown={(e) => ondividerkey(e, d)}
          ondblclick={() => setRatio(d, 0.5)}
        ></div>
      {/each}
    {/if}
  </div>

  {#if terminals.paletteFor === tab.id && active}
    <div class="palette-slot">
      <SnippetPalette hostId={tab.hostId} session={focused} onclose={closePalette} />
    </div>
  {/if}
</div>

<style>
  .terminal-view {
    position: relative;
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    background: var(--bg);
    container-type: inline-size;
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 38px;
    padding: 0 6px 0 12px;
    border-bottom: 1px solid var(--border);
    flex: none;
  }
  .where {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .where-icon {
    position: relative;
    display: flex;
    flex: none;
    color: var(--text-muted);
  }
  .where-icon .dot {
    position: absolute;
    right: -3px;
    bottom: -2px;
    display: flex;
    padding: 1.5px;
    border-radius: 50%;
    background: var(--bg);
  }
  .where-name {
    flex: none;
    color: var(--text);
    font-weight: var(--weight-medium);
  }
  .where-sep {
    color: var(--text-subtle);
  }
  .where-cwd {
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: 12px;
  }
  .panes {
    flex: none;
    height: 18px;
    font-size: 10.5px;
  }
  .spacer {
    flex: 1;
  }
  .group {
    display: flex;
    align-items: center;
    gap: 1px;
    flex: none;
  }
  .sep {
    width: 1px;
    height: 16px;
    margin: 0 4px;
    background: var(--border);
  }
  .icon-btn.on {
    background: var(--bg-active);
    color: var(--text);
  }
  .area {
    position: relative;
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }
  .area.dragging {
    cursor: col-resize;
  }
  .area.dragging.rows {
    cursor: row-resize;
  }
  .area.dragging :global(.pane) {
    pointer-events: none;
  }
  .divider {
    position: absolute;
    z-index: 8;
    background: var(--border);
  }
  .divider.row {
    width: 1px;
    margin-left: -0.5px;
    cursor: col-resize;
  }
  .divider.column {
    height: 1px;
    margin-top: -0.5px;
    cursor: row-resize;
  }
  .divider::before {
    content: "";
    position: absolute;
    inset: -3px;
  }
  .divider:hover,
  .divider:focus-visible,
  .divider.active {
    outline: none;
    background: var(--accent);
    box-shadow: 0 0 0 1px var(--accent);
  }
  .palette-slot {
    position: absolute;
    top: 38px;
    left: 0;
    right: 0;
    bottom: 0;
    pointer-events: none;
    container-type: inline-size;
  }
  .palette-slot > :global(*) {
    pointer-events: auto;
  }
</style>
