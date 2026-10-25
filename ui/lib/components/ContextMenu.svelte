<!-- Global popup menu (context menus and "more" buttons), driven by ui.menu. -->
<script lang="ts">
  import ChevronRight from "@lucide/svelte/icons/chevron-right";
  import { tick } from "svelte";
  import { ui, type MenuAction, type MenuItem, type MenuParent, type MenuSeparator, type MenuState } from "../stores/ui.svelte";

  let { menu }: { menu: MenuState } = $props();

  let el = $state<HTMLDivElement>();
  let pos = $state({ left: 0, top: 0 });
  /** The open submenu: its parent's index, and where it goes (relative to the menu). */
  let sub = $state<{ index: number; top: number; flip: boolean } | null>(null);
  let subEl = $state<HTMLDivElement>();
  const subItems = $derived.by(() => {
    const parent = sub ? menu.items[sub.index] : undefined;
    return parent && isParent(parent) ? parent.submenu : [];
  });

  const isParent = (i: MenuItem): i is MenuParent => "submenu" in i;
  const enabled = (i: MenuItem): i is MenuAction | MenuParent => !("separator" in i) && !i.disabled;

  $effect(() => {
    // Position within the viewport, flipping when there's no room.
    const { x, y } = menu;
    pos = { left: x, top: y };
    sub = null;
    void tick().then(() => {
      if (!el) return;
      const r = el.getBoundingClientRect();
      const left = Math.max(8, Math.min(x, window.innerWidth - r.width - 8));
      const top = y + r.height > window.innerHeight - 8 ? Math.max(8, y - r.height - 4) : y;
      pos = { left, top };
      el.querySelector<HTMLElement>("[role=menuitem]:not([disabled])")?.focus();
    });
  });

  function close(restore = true) {
    const target = menu.returnFocus;
    ui.closeMenu();
    if (restore && target?.isConnected) target.focus();
  }

  function select(item: MenuAction | MenuSeparator) {
    if (!enabled(item) || isParent(item)) return;
    close();
    item.onSelect();
  }

  /** Opens item `index`'s submenu next to `button`, focusing its first item if `focus`. */
  function openSub(index: number, button: HTMLElement, focus: boolean) {
    if (sub?.index !== index) sub = { index, top: button.offsetTop - 5, flip: false };
    void tick().then(() => {
      if (!sub || !subEl || !el) return;
      const r = el.getBoundingClientRect();
      const s = subEl.getBoundingClientRect();
      // No room on the right: open to the left. Keep it above the bottom edge.
      if (!sub.flip && r.right + s.width > window.innerWidth - 8) sub.flip = true;
      const overflow = s.bottom - (window.innerHeight - 8);
      if (overflow > 0) sub.top -= overflow;
      if (focus) subEl.querySelector<HTMLElement>("[role=menuitem]:not([disabled])")?.focus();
    });
  }

  /** Closes the submenu, focusing its parent item. */
  function closeSub() {
    const index = sub?.index;
    sub = null;
    if (index !== undefined) el?.querySelector<HTMLElement>(`:scope > [data-index="${index}"]`)?.focus();
  }

  function onitemclick(item: MenuItem, index: number, button: HTMLElement) {
    if (!enabled(item)) return;
    if (isParent(item)) openSub(index, button, true);
    else select(item);
  }

  function onitemhover(item: MenuItem, index: number, button: HTMLElement) {
    button.focus();
    if (enabled(item) && isParent(item)) openSub(index, button, false);
    else sub = null;
  }

  function items(container: HTMLElement | undefined): HTMLElement[] {
    return container ? [...container.querySelectorAll<HTMLElement>(":scope > [role=menuitem]:not([disabled])")] : [];
  }

  function onkeydown(e: KeyboardEvent) {
    const inSub = !!subEl?.contains(document.activeElement);
    const list = items(inSub ? subEl : el);
    const i = list.indexOf(document.activeElement as HTMLElement);
    switch (e.key) {
      case "ArrowDown":
        list[(i + 1) % list.length]?.focus();
        break;
      case "ArrowUp":
        list[(i - 1 + list.length) % list.length]?.focus();
        break;
      case "Home":
        list[0]?.focus();
        break;
      case "End":
        list[list.length - 1]?.focus();
        break;
      case "ArrowRight": {
        const button = document.activeElement as HTMLElement | null;
        const index = Number(button?.dataset.index);
        const item = menu.items[index];
        if (inSub || !button || !item || !enabled(item) || !isParent(item)) return;
        openSub(index, button, true);
        break;
      }
      case "ArrowLeft":
        if (!inSub) return;
        closeSub();
        break;
      case "Escape":
        if (inSub) closeSub();
        else close();
        break;
      case "Tab":
        close();
        break;
      default:
        return;
    }
    e.preventDefault();
    e.stopPropagation();
  }

  function onwindowpointer(e: PointerEvent) {
    if (el && !el.contains(e.target as Node)) close(false);
  }
</script>

<svelte:window onpointerdown={onwindowpointer} onblur={() => close(false)} onresize={() => close(false)} />

{#snippet entry(item: MenuAction | MenuParent)}
  {#if item.icon}
    <item.icon size={15} />
  {:else}
    <span class="icon-pad"></span>
  {/if}
  <span class="label">{item.label}</span>
  {#if isParent(item)}
    <span class="chevron"><ChevronRight size={14} /></span>
  {:else if item.hint}
    <span class="hint">{item.hint}</span>
  {/if}
{/snippet}

<div
  bind:this={el}
  class="menu"
  role="menu"
  tabindex="-1"
  aria-label={menu.label}
  style:left="{pos.left}px"
  style:top="{pos.top}px"
  {onkeydown}
  oncontextmenu={(e) => e.preventDefault()}
>
  {#each menu.items as item, i (i)}
    {#if "separator" in item}
      <div class="sep" role="separator"></div>
    {:else}
      <button
        type="button"
        role="menuitem"
        class="item"
        class:danger={"danger" in item && item.danger}
        class:open={sub?.index === i}
        disabled={item.disabled}
        tabindex="-1"
        data-index={i}
        aria-haspopup={isParent(item) ? "menu" : undefined}
        aria-expanded={isParent(item) ? sub?.index === i : undefined}
        onclick={(e) => onitemclick(item, i, e.currentTarget)}
        onpointermove={(e) => onitemhover(item, i, e.currentTarget)}
      >
        {@render entry(item)}
      </button>
    {/if}
  {/each}
  {#if sub}
    {@const parent = menu.items[sub.index]}
    <div
      bind:this={subEl}
      class="menu submenu"
      class:flip={sub.flip}
      role="menu"
      tabindex="-1"
      aria-label={"label" in parent ? parent.label : undefined}
      style:top="{sub.top}px"
    >
      {#each subItems as item, j (j)}
        {#if "separator" in item}
          <div class="sep" role="separator"></div>
        {:else}
          <button
            type="button"
            role="menuitem"
            class="item"
            class:danger={item.danger}
            disabled={item.disabled}
            tabindex="-1"
            onclick={() => select(item)}
            onpointermove={(e) => e.currentTarget.focus()}
          >
            {@render entry(item)}
          </button>
        {/if}
      {/each}
    </div>
  {/if}
</div>

<style>
  .menu {
    position: fixed;
    z-index: 1000;
    min-width: 200px;
    max-width: 300px;
    padding: 4px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    box-shadow: var(--shadow-lg);
    animation: pop var(--dur-fast) var(--ease-out);
  }
  .item {
    display: flex;
    align-items: center;
    gap: 9px;
    width: 100%;
    height: 28px;
    padding: 0 8px;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text);
    font-size: var(--text-base);
    text-align: left;
  }
  .item :global(svg) {
    color: var(--text-muted);
    flex: none;
  }
  .item:focus,
  .item:focus-visible {
    outline: none;
    background: var(--bg-active);
  }
  .item:disabled {
    opacity: 0.45;
  }
  .item.open {
    background: var(--bg-active);
  }
  .item.danger,
  .item.danger :global(svg) {
    color: var(--danger-text);
  }
  .item.danger:focus {
    background: var(--danger-soft);
  }
  .label {
    flex: 1;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .hint {
    color: var(--text-subtle);
    font-size: var(--text-xs);
  }
  .icon-pad {
    width: 15px;
  }
  .chevron {
    display: flex;
    margin-right: -3px;
  }
  .submenu {
    position: absolute;
    left: calc(100% - 4px);
    animation: none;
  }
  .submenu.flip {
    left: auto;
    right: calc(100% - 4px);
  }
  .sep {
    height: 1px;
    margin: 4px 6px;
    background: var(--border-subtle);
  }
  @keyframes pop {
    from {
      opacity: 0;
      transform: scale(0.97);
    }
  }
</style>
