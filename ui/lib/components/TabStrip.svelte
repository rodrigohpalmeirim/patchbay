<script lang="ts">
  import Columns2 from "@lucide/svelte/icons/columns-2";
  import Copy from "@lucide/svelte/icons/copy";
  import FolderOpen from "@lucide/svelte/icons/folder-open";
  import Laptop from "@lucide/svelte/icons/laptop";
  import Plus from "@lucide/svelte/icons/plus";
  import SquareTerminal from "@lucide/svelte/icons/square-terminal";
  import X from "@lucide/svelte/icons/x";
  import { hostMenuItems } from "../actions";
  import { duplicateFilesTab, openFiles } from "../files/ops";
  import { NEW_TERMINAL_KEY } from "../terminal/keys";
  import { duplicateTerminalTab, openTerminal, terminalUnavailable } from "../terminal/ops";
  import { terminals } from "../stores/terminals.svelte";
  import { connections } from "../stores/connections.svelte";
  import { dnd } from "../stores/dnd.svelte";
  import { hosts } from "../stores/hosts.svelte";
  import { tabs, type Tab } from "../stores/tabs.svelte";
  import { ui, type MenuItem } from "../stores/ui.svelte";
  import { modKey } from "../util/platform";
  import { tabDef } from "../views/tabKinds";
  import StatusDot from "./StatusDot.svelte";

  let { group, focused = false }: { group: number; focused?: boolean } = $props();

  let strip = $state<HTMLElement>();
  const list = $derived(tabs.tabsIn(group));
  const activeId = $derived(tabs.groups[group]?.activeId ?? null);

  function focusTab(id: string) {
    tabs.activate(id);
    document.querySelector<HTMLElement>(`[data-tab-id="${CSS.escape(id)}"]`)?.focus();
  }

  function onkeydown(e: KeyboardEvent, tab: Tab) {
    const i = list.indexOf(tab);
    let next: Tab | undefined;
    if (e.key === "ArrowRight") next = list[(i + 1) % list.length];
    else if (e.key === "ArrowLeft") next = list[(i - 1 + list.length) % list.length];
    else if (e.key === "Home") next = list[0];
    else if (e.key === "End") next = list[list.length - 1];
    else if (e.key === "Delete") {
      e.preventDefault();
      tabs.close(tab.id);
      if (tabs.activeId) focusTab(tabs.activeId);
      return;
    } else return;
    e.preventDefault();
    if (next) focusTab(next.id);
  }

  function oncontextmenu(e: MouseEvent, tab: Tab) {
    e.preventDefault();
    const host = hosts.get(tab.hostId);
    const items: MenuItem[] = [
      { label: "Close tab", icon: X, hint: `${modKey}+W`, onSelect: () => tabs.close(tab.id) },
      {
        label: "Close other tabs",
        disabled: list.length < 2,
        onSelect: () => tabs.closeOthers(tab.id),
      },
      { separator: true },
    ];
    const canMove = tabs.isSplit || list.length > 1;
    if (canMove) {
      items.push({
        label: tabs.isSplit ? "Move to other side" : "Open in split view",
        icon: Columns2,
        hint: `${modKey}+\\`,
        onSelect: () => tabs.moveToGroup(tab.id, "other"),
      });
    }
    if (tab.kind === "terminal") {
      items.push({ label: "Duplicate tab", icon: Copy, onSelect: () => duplicateTerminalTab(tab, group) });
      if (!canMove) {
        items.push({ label: "Duplicate in split view", icon: Columns2, onSelect: () => duplicateTerminalTab(tab, group === 0 ? 1 : 0) });
      }
    }
    if (tab.kind === "files") {
      items.push({ label: "Duplicate tab", icon: Copy, onSelect: () => duplicateFilesTab(tab, group) });
      if (!canMove) {
        items.push({ label: "Duplicate in split view", icon: Columns2, onSelect: () => duplicateFilesTab(tab, group === 0 ? 1 : 0) });
      }
    }
    if (host) items.push({ separator: true }, ...hostMenuItems(host, { includeOpen: tab.kind !== "host" }));
    ui.openMenu("Tab actions", items, { x: e.clientX, y: e.clientY });
  }

  function onpointerdown(e: PointerEvent, tab: Tab) {
    if (e.button !== 0) return;
    dnd.armTabDrag(e, tab.id);
  }

  function onclick(tab: Tab) {
    if (dnd.justDropped) return;
    tabs.activate(tab.id);
    // Clicking a terminal tab means "type here".
    if (tab.kind === "terminal") terminals.requestFocus(tab.id);
  }

  /** "+" menu: new terminal / files tab for this group's machine, or this computer. */
  function newMenu(e: MouseEvent) {
    const activeTab = activeId ? tabs.get(activeId) : undefined;
    const hostId = activeTab?.hostId ?? null;
    const host = hosts.get(hostId);
    const items: MenuItem[] = [];
    if (host) {
      const noShell = terminalUnavailable(host.id);
      const noSftp = connections.get(host.id).capabilities?.sftp === false;
      items.push(
        {
          label: `Terminal on ${host.name}`,
          icon: SquareTerminal,
          hint: noShell ? "SFTP only" : group === tabs.focused ? NEW_TERMINAL_KEY : undefined,
          disabled: !!noShell,
          onSelect: () => openTerminal(host.id, { group }),
        },
        { label: `Files on ${host.name}`, icon: FolderOpen, disabled: noSftp, onSelect: () => openFiles(host.id, { group }) },
        { separator: true },
      );
    }
    items.push(
      { label: "Local terminal", icon: SquareTerminal, hint: !host && group === tabs.focused ? NEW_TERMINAL_KEY : undefined, onSelect: () => openTerminal(null, { group }) },
      { label: "Local files", icon: Laptop, onSelect: () => openFiles(null, { group }) },
    );
    ui.openMenu("New tab", items, e.currentTarget as HTMLElement);
  }

  // Keep the active tab scrolled into view.
  $effect(() => {
    const id = activeId;
    if (!id || !strip) return;
    strip.querySelector(`[data-tab-id="${CSS.escape(id)}"]`)?.scrollIntoView({ block: "nearest", inline: "nearest" });
  });
</script>

<div class="strip" class:focused bind:this={strip} role="tablist" aria-label={tabs.isSplit ? (group === 0 ? "Left tabs" : "Right tabs") : "Open tabs"} data-strip-group={group}>
  {#each list as tab (tab.id)}
    {@const host = hosts.get(tab.hostId)}
    {@const def = tabDef(tab)}
    {@const Icon = def.icon(tab)}
    {@const active = tab.id === activeId}
    {@const title = def.title(tab, host)}
    {@const subtitle = def.subtitle?.(tab, host)}
    <div class="tab" class:active class:dragging={dnd.tab?.id === tab.id} data-tab-wrap>
      <button
        type="button"
        role="tab"
        class="tab-btn"
        id="tab-{tab.id}"
        data-tab-id={tab.id}
        aria-selected={active}
        aria-controls="panel-{tab.id}"
        tabindex={active ? 0 : -1}
        title={def.tooltip?.(tab, host) ?? title}
        onclick={() => onclick(tab)}
        onpointerdown={(e) => onpointerdown(e, tab)}
        onauxclick={(e) => e.button === 1 && tabs.close(tab.id)}
        onkeydown={(e) => onkeydown(e, tab)}
        oncontextmenu={(e) => oncontextmenu(e, tab)}
      >
        <span class="icon">
          <Icon size={14} />
          {#if tab.hostId}
            <span class="badge-dot"><StatusDot status={connections.status(tab.hostId)} size={6} /></span>
          {/if}
        </span>
        <span class="title">{title}</span>
        {#if subtitle}<span class="subtitle">{subtitle}</span>{/if}
      </button>
      <button
        type="button"
        class="close icon-btn sm"
        tabindex="-1"
        aria-label="Close {title}"
        title="Close ({modKey}+W)"
        onclick={() => tabs.close(tab.id)}
      >
        <X size={13} />
      </button>
    </div>
  {/each}
  <button type="button" class="new icon-btn" aria-label="New tab" aria-haspopup="menu" title="New terminal or files tab" onclick={newMenu}>
    <Plus size={15} />
  </button>
  <span class="rest" aria-hidden="true"></span>
</div>

<style>
  .strip {
    display: flex;
    align-items: flex-end;
    gap: 2px;
    height: var(--tabstrip-h);
    padding: 0 8px;
    overflow-x: auto;
    overflow-y: hidden;
    scrollbar-width: none;
    border-bottom: 1px solid var(--border);
    background: var(--bg-sidebar);
    flex: none;
  }
  .strip::-webkit-scrollbar {
    display: none;
  }
  .rest {
    flex: 1;
    align-self: stretch;
  }
  .new {
    flex: none;
    align-self: center;
    width: 26px;
    height: 26px;
    margin: 2px 0 0 2px;
  }
  .tab {
    position: relative;
    display: flex;
    align-items: center;
    flex: 0 1 210px;
    min-width: 110px;
    height: 32px;
    border: 1px solid transparent;
    border-bottom: 0;
    border-radius: var(--radius-sm) var(--radius-sm) 0 0;
    color: var(--text-muted);
    transition: background var(--dur-fast);
  }
  .tab:hover:not(.active) {
    background: var(--bg-hover);
  }
  .tab.active {
    background: var(--bg);
    border-color: var(--border);
    color: var(--text);
  }
  .tab.dragging {
    opacity: 0.5;
  }
  /* In split view the focused group's active tab carries an accent cap. */
  .focused .tab.active::before {
    content: "";
    position: absolute;
    left: -1px;
    right: -1px;
    top: -1px;
    height: 2px;
    border-radius: var(--radius-sm) var(--radius-sm) 0 0;
    background: var(--accent);
  }
  /* Covers the strip's bottom border under the active tab. */
  .tab.active::after {
    content: "";
    position: absolute;
    left: 0;
    right: 0;
    bottom: -1px;
    height: 1px;
    background: var(--bg);
  }
  .tab-btn {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 1;
    min-width: 0;
    height: 100%;
    padding: 0 4px 0 10px;
    border: 0;
    background: transparent;
    color: inherit;
    font-size: var(--text-base);
    text-align: left;
  }
  .tab-btn:focus-visible {
    outline-offset: -3px;
    border-radius: var(--radius-sm);
  }
  .icon {
    position: relative;
    display: flex;
    flex: none;
  }
  .badge-dot {
    position: absolute;
    right: -3px;
    bottom: -2px;
    display: flex;
    padding: 1.5px;
    border-radius: 50%;
    background: var(--bg-sidebar);
  }
  .active .badge-dot {
    background: var(--bg);
  }
  .title {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .subtitle {
    flex: 1 10 auto;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--text-subtle);
    font-size: var(--text-sm);
  }
  .close {
    margin-right: 5px;
    opacity: 0;
  }
  .tab:hover .close,
  .tab.active .close,
  .close:focus-visible {
    opacity: 1;
  }
</style>
