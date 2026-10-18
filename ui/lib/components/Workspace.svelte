<!--
  Main area: one or two tab groups side by side. Strips are rendered per
  group; panels for *all* tabs live in one keyed list and are positioned into
  their group's column, so moving a tab between groups never remounts it.
-->
<script lang="ts">
  import { dnd } from "../stores/dnd.svelte";
  import { hosts } from "../stores/hosts.svelte";
  import { tabs } from "../stores/tabs.svelte";
  import { tabDef } from "../views/tabKinds";
  import TabStrip from "./TabStrip.svelte";

  let panels = $state<HTMLElement>();
  let resizing = $state(false);

  function onresizestart(e: PointerEvent) {
    if (!panels) return;
    e.preventDefault();
    resizing = true;
    const rect = panels.getBoundingClientRect();
    const target = e.currentTarget as HTMLElement;
    target.setPointerCapture(e.pointerId);
    const move = (ev: PointerEvent) => tabs.setSplitRatio((ev.clientX - rect.left) / rect.width);
    const up = () => {
      resizing = false;
      target.removeEventListener("pointermove", move);
      target.removeEventListener("pointerup", up);
    };
    target.addEventListener("pointermove", move);
    target.addEventListener("pointerup", up);
  }

  function onresizekey(e: KeyboardEvent) {
    if (e.key === "ArrowLeft") tabs.setSplitRatio(tabs.splitRatio - 0.03);
    else if (e.key === "ArrowRight") tabs.setSplitRatio(tabs.splitRatio + 0.03);
    else if (e.key === "Home") tabs.setSplitRatio(0.5);
    else return;
    e.preventDefault();
  }

  const tabDrag = $derived(dnd.tab);
  const dragged = $derived(tabDrag ? tabs.get(tabDrag.id) : undefined);
  const draggedTitle = $derived(dragged ? tabDef(dragged).title(dragged, hosts.get(dragged.hostId)) : "");
</script>

<div class="workspace" class:split={tabs.isSplit} class:resizing style:--split={tabs.splitRatio}>
  <div class="strips">
    {#each tabs.groups as g, gi (g.id)}
      <div class="strip-slot" class:second={gi === 1}>
        <TabStrip group={gi} focused={tabs.isSplit && tabs.focused === gi} />
      </div>
    {/each}
  </div>
  <div class="panels" bind:this={panels} data-panels>
    {#each tabs.list as tab (tab.id)}
      {@const gi = tabs.groupOf(tab.id)}
      {@const host = hosts.get(tab.hostId)}
      {@const def = tabDef(tab)}
      {@const visible = tabs.groups[gi]?.activeId === tab.id}
      <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
      <div
        class="panel"
        class:second={tabs.isSplit && gi === 1}
        class:first={tabs.isSplit && gi === 0}
        role="tabpanel"
        id="panel-{tab.id}"
        aria-labelledby="tab-{tab.id}"
        tabindex="-1"
        hidden={!visible}
        onpointerdowncapture={() => tabs.focusGroup(tabs.groupOf(tab.id))}
        onfocusin={() => tabs.focusGroup(tabs.groupOf(tab.id))}
      >
        {#if tab.hostId === null || host}
          <def.component tab={tab as never} host={(host ?? null) as never} active={visible} />
        {/if}
      </div>
    {/each}

    {#if tabDrag?.target?.kind === "split"}
      <div class="drop-zone right"><span>Open in split view</span></div>
    {:else if tabDrag?.target?.kind === "group"}
      <div class="drop-zone" class:right={tabDrag.target.group === 1} class:left={tabDrag.target.group === 0}>
        <span>Move here</span>
      </div>
    {/if}
  </div>

  {#if tabs.isSplit}
    <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
    <div
      class="divider"
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize split"
      aria-valuenow={Math.round(tabs.splitRatio * 100)}
      aria-valuemin={20}
      aria-valuemax={80}
      tabindex="0"
      onpointerdown={onresizestart}
      onkeydown={onresizekey}
      ondblclick={() => tabs.setSplitRatio(0.5)}
    ></div>
  {/if}
</div>

{#if tabDrag}
  {#if tabDrag.target?.kind === "strip"}
    {@const strip = document.querySelector(`[data-strip-group="${tabDrag.target.group}"]`)?.getBoundingClientRect()}
    {#if strip}
      <div class="insert-line" style:left="{tabDrag.target.x - 1}px" style:top="{strip.top + 6}px" style:height="{strip.height - 6}px"></div>
    {/if}
  {/if}
  <div class="tab-ghost" style:left="{tabDrag.x + 12}px" style:top="{tabDrag.y + 10}px">{draggedTitle}</div>
{/if}

<style>
  .workspace {
    position: relative;
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
    min-width: 0;
  }
  .strips {
    display: flex;
    flex: none;
    min-width: 0;
  }
  .strip-slot {
    flex: 1;
    min-width: 0;
  }
  .split .strip-slot:not(.second) {
    flex: none;
    width: calc(var(--split) * 100%);
  }
  .panels {
    position: relative;
    flex: 1;
    min-height: 0;
  }
  .panel {
    position: absolute;
    inset: 0;
    outline: none;
  }
  .panel.first {
    right: auto;
    width: calc(var(--split) * 100%);
  }
  .panel.second {
    left: calc(var(--split) * 100% + 1px);
  }
  .panel[hidden] {
    display: none;
  }
  .divider {
    position: absolute;
    top: 0;
    bottom: 0;
    left: calc(var(--split) * 100%);
    width: 1px;
    background: var(--border);
    z-index: 6;
    cursor: col-resize;
  }
  .divider::before {
    content: "";
    position: absolute;
    inset: 0 -3px;
  }
  .divider:hover,
  .divider:focus-visible,
  .resizing .divider {
    outline: none;
    background: var(--accent);
    box-shadow: 0 0 0 1px var(--accent);
  }
  .resizing {
    cursor: col-resize;
  }
  .resizing .panels {
    pointer-events: none;
  }
  .drop-zone {
    position: absolute;
    top: 6px;
    bottom: 6px;
    left: 6px;
    right: 6px;
    display: grid;
    place-items: center;
    border: 2px dashed var(--accent);
    border-radius: var(--radius-lg);
    background: var(--accent-soft);
    pointer-events: none;
    z-index: 8;
  }
  .drop-zone.right {
    left: calc(50% + 3px);
  }
  .split .drop-zone.right {
    left: calc(var(--split) * 100% + 6px);
  }
  .split .drop-zone.left {
    right: calc((1 - var(--split)) * 100% + 6px);
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
