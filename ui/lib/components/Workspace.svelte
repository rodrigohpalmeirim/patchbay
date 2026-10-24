<!--
  Main area: tab groups laid out by the split tree in stores/tabs.svelte.ts.
  Strips are rendered per group; panels for *all* tabs live in one keyed list
  and are positioned into their group's rect, so moving a tab between groups
  never remounts it. Groups after a divider start 1px later; the divider
  fills that gap.
-->
<script lang="ts">
  import { dnd } from "../stores/dnd.svelte";
  import { hosts } from "../stores/hosts.svelte";
  import { GROUP_MIN_H, GROUP_MIN_W, tabs } from "../stores/tabs.svelte";
  import { computeLayout, findSplit, type Divider, type Rect } from "../util/splitTree";
  import { tabDef } from "../views/tabKinds";
  import TabStrip from "./TabStrip.svelte";

  let area = $state<HTMLElement>();
  let resizing = $state<Divider | null>(null);

  const layout = $derived(computeLayout(tabs.layout));
  const FULL: Rect = { x: 0, y: 0, w: 1, h: 1 };

  const pct = (f: number) => `${f * 100}%`;
  /** 1px for the divider before a rect that doesn't start at the edge. */
  const gap = (f: number) => (f > 1e-6 ? 1 : 0);

  function stripStyle(r: Rect = FULL): string {
    return `left:calc(${pct(r.x)} + ${gap(r.x)}px);width:calc(${pct(r.w)} - ${gap(r.x)}px);top:calc(${pct(r.y)} + ${gap(r.y)}px)`;
  }

  function panelStyle(r: Rect = FULL): string {
    const top = `(${gap(r.y)}px + var(--tabstrip-h))`;
    return `left:calc(${pct(r.x)} + ${gap(r.x)}px);width:calc(${pct(r.w)} - ${gap(r.x)}px);top:calc(${pct(r.y)} + ${top});height:calc(${pct(r.h)} - ${top})`;
  }

  function dividerStyle(d: Divider): string {
    const a = d.area;
    if (d.dir === "row") return `left:${pct(a.x + a.w * d.ratio)};top:${pct(a.y)};height:${pct(a.h)}`;
    return `top:${pct(a.y + a.h * d.ratio)};left:${pct(a.x)};width:${pct(a.w)}`;
  }

  /** Sets a split's ratio, keeping both sides at least the minimum group size. */
  function setRatio(d: Divider, ratio: number) {
    if (!area) return;
    const total = d.dir === "row" ? area.clientWidth * d.area.w : area.clientHeight * d.area.h;
    const minPx = d.dir === "row" ? GROUP_MIN_W : GROUP_MIN_H;
    const min = total > 0 ? Math.min(0.5, minPx / total) : 0.1;
    tabs.setRatio(d.splitId, Math.min(1 - min, Math.max(min, ratio)));
  }

  function onresizestart(e: PointerEvent, d: Divider) {
    if (!area || e.button !== 0) return;
    e.preventDefault();
    resizing = d;
    const rect = area.getBoundingClientRect();
    const target = e.currentTarget as HTMLElement;
    target.setPointerCapture(e.pointerId);
    const move = (ev: PointerEvent) => {
      const r =
        d.dir === "row"
          ? (ev.clientX - rect.left - rect.width * d.area.x) / (rect.width * d.area.w)
          : (ev.clientY - rect.top - rect.height * d.area.y) / (rect.height * d.area.h);
      setRatio(d, r);
    };
    const up = () => {
      resizing = null;
      target.removeEventListener("pointermove", move);
      target.removeEventListener("pointerup", up);
    };
    target.addEventListener("pointermove", move);
    target.addEventListener("pointerup", up);
  }

  function onresizekey(e: KeyboardEvent, d: Divider) {
    const ratio = findSplit(tabs.layout, d.splitId)?.ratio ?? d.ratio;
    const [less, more] = d.dir === "row" ? ["ArrowLeft", "ArrowRight"] : ["ArrowUp", "ArrowDown"];
    if (e.key === less) setRatio(d, ratio - 0.03);
    else if (e.key === more) setRatio(d, ratio + 0.03);
    else if (e.key === "Home") setRatio(d, 0.5);
    else return;
    e.preventDefault();
  }

  const tabDrag = $derived(dnd.tab);
  const dragged = $derived(tabDrag ? tabs.get(tabDrag.id) : undefined);
  const draggedTitle = $derived(dragged ? tabDef(dragged).title(dragged, hosts.get(dragged.hostId)) : "");
</script>

<div
  class="workspace"
  class:resizing={!!resizing}
  class:rows={resizing?.dir === "column"}
  bind:this={area}
>
  {#each tabs.groups as g (g.id)}
    <div class="strip-slot" style={stripStyle(layout.rects.get(g.id))}>
      <TabStrip group={g.id} focused={tabs.isSplit && tabs.focusedGroup === g.id} />
    </div>
  {/each}

  {#each tabs.list as tab (tab.id)}
    {@const gid = tabs.groupOf(tab.id)}
    {@const host = hosts.get(tab.hostId)}
    {@const def = tabDef(tab)}
    {@const visible = !!gid && tabs.group(gid)?.activeId === tab.id}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div
      class="panel"
      style={panelStyle(gid ? layout.rects.get(gid) : undefined)}
      role="tabpanel"
      id="panel-{tab.id}"
      aria-labelledby="tab-{tab.id}"
      tabindex="-1"
      hidden={!visible}
      data-group-panel={visible ? gid : undefined}
      onpointerdowncapture={() => tabs.focusGroup(tabs.groupOf(tab.id))}
      onfocusin={() => tabs.focusGroup(tabs.groupOf(tab.id))}
    >
      {#if tab.hostId === null || host}
        <def.component tab={tab as never} host={(host ?? null) as never} active={visible} />
      {/if}
    </div>
  {/each}

  {#each layout.dividers as d (d.splitId)}
    <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
    <div
      class="divider {d.dir}"
      class:active={resizing?.splitId === d.splitId}
      style={dividerStyle(d)}
      role="separator"
      aria-orientation={d.dir === "row" ? "vertical" : "horizontal"}
      aria-label="Resize tab groups"
      aria-valuenow={Math.round(d.ratio * 100)}
      aria-valuemin={0}
      aria-valuemax={100}
      tabindex="0"
      onpointerdown={(e) => onresizestart(e, d)}
      onkeydown={(e) => onresizekey(e, d)}
      ondblclick={() => setRatio(d, 0.5)}
    ></div>
  {/each}
</div>

{#if tabDrag}
  {#if tabDrag.target?.kind === "strip"}
    {@const strip = document.querySelector(`[data-strip-group="${tabDrag.target.group}"]`)?.getBoundingClientRect()}
    {#if strip}
      <div class="insert-line" style:left="{tabDrag.target.x - 1}px" style:top="{strip.top + 6}px" style:height="{strip.height - 6}px"></div>
    {/if}
  {:else if tabDrag.target?.kind === "group"}
    {@const r = tabDrag.target.rect}
    <div class="drop-zone" style:left="{r.left + 6}px" style:top="{r.top + 6}px" style:width="{r.width - 12}px" style:height="{r.height - 12}px">
      <span>{tabDrag.target.edge ? "Split" : "Move here"}</span>
    </div>
  {/if}
  <div class="tab-ghost" style:left="{tabDrag.x + 12}px" style:top="{tabDrag.y + 10}px">{draggedTitle}</div>
{/if}

<style>
  .workspace {
    position: relative;
    flex: 1;
    min-height: 0;
    min-width: 0;
  }
  .strip-slot {
    position: absolute;
    z-index: 1;
    min-width: 0;
  }
  .panel {
    position: absolute;
    overflow: hidden;
    outline: none;
  }
  .panel[hidden] {
    display: none;
  }
  .divider {
    position: absolute;
    z-index: 6;
    background: var(--border);
  }
  .divider.row {
    width: 1px;
    cursor: col-resize;
  }
  .divider.column {
    height: 1px;
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
  .resizing {
    cursor: col-resize;
  }
  .resizing.rows {
    cursor: row-resize;
  }
  .resizing .panel {
    pointer-events: none;
  }
  .drop-zone {
    position: fixed;
    display: grid;
    place-items: center;
    border: 2px dashed var(--accent);
    border-radius: var(--radius-lg);
    background: var(--accent-soft);
    pointer-events: none;
    z-index: 1000;
    transition:
      left var(--dur-fast),
      top var(--dur-fast),
      width var(--dur-fast),
      height var(--dur-fast);
  }
  .drop-zone span {
    padding: 6px 12px;
    border-radius: var(--radius-full);
    background: var(--accent);
    color: var(--text-on-accent);
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
  }
  .insert-line {
    position: fixed;
    width: 2px;
    border-radius: 1px;
    background: var(--accent);
    pointer-events: none;
    z-index: 1001;
  }
  .tab-ghost {
    position: fixed;
    z-index: 1001;
    max-width: 220px;
    padding: 4px 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-elevated);
    box-shadow: var(--shadow-md);
    font-size: var(--text-sm);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    pointer-events: none;
  }
</style>
