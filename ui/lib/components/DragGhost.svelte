<!--
  Follows the pointer while file rows are dragged; shows what a drop would
  do. Near and beyond the window edge it reports whether the entries can be
  dragged out to the OS yet (outside the window it stays pinned to the edge).
-->
<script lang="ts">
  import Ban from "@lucide/svelte/icons/ban";
  import Copy from "@lucide/svelte/icons/copy";
  import LoaderCircle from "@lucide/svelte/icons/loader-circle";
  import MoveRight from "@lucide/svelte/icons/move-right";
  import { COPY_KEY_LABEL, dnd, dropAllowed, dropIsMove } from "../stores/dnd.svelte";
  import { iconFor } from "../util/fileIcons";
  import { plural } from "../util/format";

  const EDGE = 40;

  const drag = $derived(dnd.file);
  const out = $derived(dnd.out);
  const allowed = $derived(drag ? dropAllowed(drag) : false);
  const move = $derived(drag ? dropIsMove(drag) : false);
  const sameMachine = $derived(!!drag?.target && drag.target.hostId === drag.hostId);

  let vw = $state(1200);
  let vh = $state(800);
  const nearEdge = $derived(!!drag && (drag.x < EDGE || drag.y < EDGE || drag.x > vw - EDGE || drag.y > vh - EDGE));
  const items = $derived(drag ? plural(drag.entries.length, "item") : "");
  const pct = $derived(out?.progress != null ? ` ${Math.floor(out.progress * 100)}%` : "");

  /** Drag-out status line, shown near the edge and outside the window. */
  const outLine = $derived.by(() => {
    if (!drag || !out || !(drag.outside || nearEdge)) return null;
    switch (out.status) {
      case "ready":
        return drag.outside ? null : "Drag out of Patchbay to copy to your computer";
      case "preparing":
        return `Preparing ${items} for your computer…${pct}`;
      case "tooLarge":
        return "Too large to drag out (over 200 MB) — use Download to…";
      case "failed":
        return out.error ? `Can’t drag out: ${out.error}` : "Can’t drag out these items";
    }
  });

  /** Below-right of the pointer; flipped near the right/bottom edges, pinned outside. */
  const pos = $derived.by(() => {
    if (!drag) return { left: null, right: null, top: null, bottom: null };
    const { x, y } = drag;
    const h = x >= vw ? { left: null, right: 8 } : x + 340 > vw ? { left: null, right: vw - x + 10 } : { left: Math.max(8, x + 14), right: null };
    const v = y >= vh ? { top: null, bottom: 8 } : y + 80 > vh ? { top: null, bottom: vh - y + 10 } : { top: Math.max(8, y + 12), bottom: null };
    return { ...h, ...v };
  });
  const px = (n: number | null) => (n == null ? null : `${n}px`);
</script>

<svelte:window bind:innerWidth={vw} bind:innerHeight={vh} />

{#if drag}
  {@const first = drag.entries[0]}
  {@const ic = iconFor(first)}
  <div
    class="wrap"
    class:flip-x={pos.right != null}
    class:flip-y={pos.bottom != null}
    style:left={px(pos.left)}
    style:right={px(pos.right)}
    style:top={px(pos.top)}
    style:bottom={px(pos.bottom)}
    aria-hidden="true"
  >
    <div class="ghost">
      <span class="stack" class:many={drag.entries.length > 1}><ic.icon size={15} /></span>
      <span class="name">{drag.entries.length === 1 ? first.name : items}</span>
      {#if drag.target}
        <span class="op" class:no={!allowed}>
          {#if !allowed}<Ban size={11} /> Can’t drop here{:else if move}<MoveRight size={11} /> Move{:else}<Copy size={11} /> Copy{/if}
        </span>
      {:else if drag.outside && (out?.status === "tooLarge" || out?.status === "failed")}
        <span class="op no"><Ban size={11} /> Can’t drag out</span>
      {/if}
    </div>
    {#if outLine}
      <div class="hint" class:warn={out?.status === "tooLarge" || out?.status === "failed"}>
        {#if out?.status === "preparing"}<LoaderCircle size={10} class="spin" />{/if}
        {outLine}
      </div>
    {:else if sameMachine && allowed}
      <div class="hint">Hold {COPY_KEY_LABEL} to {move ? "copy" : "move"}</div>
    {/if}
  </div>
{/if}

<style>
  .wrap {
    position: fixed;
    z-index: 1002;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 4px;
    max-width: 340px;
    pointer-events: none;
  }
  .wrap.flip-x {
    align-items: flex-end;
  }
  .wrap.flip-y {
    flex-direction: column-reverse;
  }
  .ghost {
    display: flex;
    align-items: center;
    gap: 8px;
    max-width: 300px;
    height: 30px;
    padding: 0 6px 0 9px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-elevated);
    box-shadow: var(--shadow-lg);
    font-size: var(--text-sm);
    pointer-events: none;
  }
  .stack {
    position: relative;
    display: flex;
    color: var(--accent-text);
  }
  .stack.many::after {
    content: "";
    position: absolute;
    inset: -2px -3px -2px -2px;
    border-right: 2px solid var(--accent-soft);
    border-bottom: 2px solid var(--accent-soft);
    border-radius: 3px;
  }
  .name {
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-weight: var(--weight-medium);
  }
  .op {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex: none;
    height: 20px;
    padding: 0 7px;
    border-radius: var(--radius-full);
    background: var(--accent);
    color: var(--text-on-accent);
    font-size: var(--text-xs);
    font-weight: var(--weight-medium);
  }
  .op.no {
    background: var(--bg-active);
    color: var(--text-muted);
  }
  .hint {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    max-width: 100%;
    font-variant-numeric: tabular-nums;
    padding: 2px 7px;
    border-radius: var(--radius-xs);
    background: var(--bg-elevated);
    border: 1px solid var(--border-subtle);
    color: var(--text-muted);
    font-size: var(--text-xs);
    pointer-events: none;
  }
  .hint.warn {
    border-color: transparent;
    background: var(--warning-soft);
    color: var(--warning-text);
  }
  :global(body.dragging),
  :global(body.dragging *) {
    cursor: grabbing !important;
  }
</style>
