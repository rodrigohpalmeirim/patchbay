<!--
  Feedback while a tab (or terminal pane) is dragged: where it would go (an
  insert line in a tab strip, or a highlighted area with what happens) and a
  ghost with its title by the pointer. Also for tabs of other windows
  dragged over this one (`dnd.incoming`).
-->
<script lang="ts">
  import { dnd } from "../stores/dnd.svelte";
  import { hosts } from "../stores/hosts.svelte";
  import { tabs } from "../stores/tabs.svelte";
  import { paneTitle } from "../terminal/ops";
  import { tabDef } from "../views/tabKinds";

  const own = $derived(dnd.tab);
  const incoming = $derived(dnd.incoming);
  /** Outside the window (moving or tearing the tab off) only the window under the pointer shows it. */
  const drag = $derived(own ? (own.outside ? null : own) : incoming);
  const title = $derived.by(() => {
    if (incoming && drag === incoming) return incoming.tab.title;
    const tab = own ? tabs.get(own.id) : undefined;
    if (!tab) return "";
    if (own?.pane && tab.kind === "terminal") return paneTitle(tab, own.pane);
    return tabDef(tab).title(tab, hosts.get(tab.hostId));
  });
  let vw = $state(1200);
  let vh = $state(800);
  /** Ghost below-right of the pointer; flipped near the right/bottom edges (max-width + room). */
  const flipX = $derived(!!drag && drag.x + 250 > vw);
  const flipY = $derived(!!drag && drag.y + 40 > vh);

  const label = $derived.by(() => {
    const t = drag?.target;
    if (!t || t.kind === "strip") return "";
    if (t.kind === "window") return "Move to this window";
    if (t.kind === "pane") return "Add as pane";
    if (t.edge) return "Split view";
    return own?.pane ? "New tab here" : "Move here";
  });
</script>

<svelte:window bind:innerWidth={vw} bind:innerHeight={vh} />

{#if drag}
  {#if drag.target?.kind === "strip"}
    {@const strip = document.querySelector(`[data-strip-group="${drag.target.group}"]`)?.getBoundingClientRect()}
    {#if strip}
      <div class="insert-line" style:left="{drag.target.x - 1}px" style:top="{strip.top + 6}px" style:height="{strip.height - 6}px"></div>
    {/if}
  {:else if drag.target}
    {@const r = drag.target.rect}
    <div class="drop-zone" style:left="{r.left + 6}px" style:top="{r.top + 6}px" style:width="{r.width - 12}px" style:height="{r.height - 12}px">
      <span>{label}</span>
    </div>
  {/if}
  <div
    class="tab-ghost"
    style:left={flipX ? null : `${drag.x + 12}px`}
    style:right={flipX ? `${vw - drag.x + 8}px` : null}
    style:top={flipY ? null : `${drag.y + 10}px`}
    style:bottom={flipY ? `${vh - drag.y + 8}px` : null}
  >
    {title}
  </div>
{/if}

<style>
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
