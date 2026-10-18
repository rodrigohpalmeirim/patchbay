<!-- Global popup menu (context menus and "more" buttons), driven by ui.menu. -->
<script lang="ts">
  import { tick } from "svelte";
  import { ui, type MenuItem, type MenuState } from "../stores/ui.svelte";

  let { menu }: { menu: MenuState } = $props();

  let el = $state<HTMLDivElement>();
  let pos = $state({ left: 0, top: 0 });

  const enabled = (i: MenuItem): i is Extract<MenuItem, { label: string }> =>
    !("separator" in i) && !i.disabled;

  $effect(() => {
    // Position within the viewport, flipping when there's no room.
    const { x, y } = menu;
    pos = { left: x, top: y };
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

  function select(item: MenuItem) {
    if (!enabled(item)) return;
    close();
    item.onSelect();
  }

  function items(): HTMLElement[] {
    return el ? [...el.querySelectorAll<HTMLElement>("[role=menuitem]:not([disabled])")] : [];
  }

  function onkeydown(e: KeyboardEvent) {
    const list = items();
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
      case "Escape":
        close();
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
        class:danger={item.danger}
        disabled={item.disabled}
        tabindex="-1"
        onclick={() => select(item)}
        onpointermove={(e) => e.currentTarget.focus()}
      >
        {#if item.icon}
          <item.icon size={15} />
        {:else}
          <span class="icon-pad"></span>
        {/if}
        <span class="label">{item.label}</span>
        {#if item.hint}<span class="hint">{item.hint}</span>{/if}
      </button>
    {/if}
  {/each}
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
