<script lang="ts">
  import ChevronRight from "@lucide/svelte/icons/chevron-right";
  import Plus from "@lucide/svelte/icons/plus";
  import Search from "@lucide/svelte/icons/search";
  import Settings from "@lucide/svelte/icons/settings";
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";
  import X from "@lucide/svelte/icons/x";
  import { onMount } from "svelte";
  import { newHost, openHost } from "../actions";
  import { connections } from "../stores/connections.svelte";
  import { edits } from "../stores/edits.svelte";
  import { hosts } from "../stores/hosts.svelte";
  import { settings } from "../stores/settings.svelte";
  import { tabs } from "../stores/tabs.svelte";
  import { ui } from "../stores/ui.svelte";
  import type { Host } from "../types";
  import { collator } from "../util/format";
  import { matchHost } from "../util/fuzzy";
  import { modKey } from "../util/platform";
  import Laptop from "@lucide/svelte/icons/laptop";
  import Columns2 from "@lucide/svelte/icons/columns-2";
  import FolderOpen from "@lucide/svelte/icons/folder-open";
  import { openFiles } from "../files/ops";
  import { openTerminal } from "../terminal/ops";
  import SquareTerminal from "@lucide/svelte/icons/square-terminal";
  import HostRow from "./HostRow.svelte";
  import EditIndicator from "./EditIndicator.svelte";
  import TransferIndicator from "./TransferIndicator.svelte";
  import Cable from "@lucide/svelte/icons/cable";
  import { forwards } from "../stores/forwards.svelte";

  interface Section {
    key: string;
    label: string;
    hosts: { host: Host; highlight: number[] }[];
  }

  let query = $state("");
  let searchEl = $state<HTMLInputElement>();
  let listEl = $state<HTMLElement>();
  let navKey = $state<string | null>(null);

  const searching = $derived(query.trim().length > 0);

  const sections = $derived.by((): Section[] => {
    const matched: { host: Host; score: number; highlight: number[] }[] = [];
    for (const host of hosts.list) {
      const m = matchHost(host, query);
      if (m) matched.push({ host, score: m.score, highlight: m.nameIndices });
    }
    const sort = (a: (typeof matched)[number], b: (typeof matched)[number]) =>
      searching ? b.score - a.score || collator.compare(a.host.name, b.host.name) : collator.compare(a.host.name, b.host.name);

    // Hosts from ssh_config (the user's and Patchbay's) in one list unless
    // grouped, then the devices on the tailnet.
    const ungrouped: typeof matched = [];
    const groups = new Map<string, typeof matched>();
    const tailnet: typeof matched = [];
    for (const m of matched) {
      if (m.host.source === "tailscale") tailnet.push(m);
      else if (m.host.group) groups.set(m.host.group, [...(groups.get(m.host.group) ?? []), m]);
      else ungrouped.push(m);
    }
    const out: Section[] = [];
    if (ungrouped.length) out.push({ key: "hosts", label: "Hosts", hosts: ungrouped.sort(sort) });
    for (const g of [...groups.keys()].sort(collator.compare)) {
      out.push({ key: `group:${g}`, label: g, hosts: groups.get(g)!.sort(sort) });
    }
    if (tailnet.length) {
      const online = (m: (typeof matched)[number]) => (m.host.tailscale?.online ? 0 : 1);
      const byOnline = (a: (typeof matched)[number], b: (typeof matched)[number]) =>
        searching ? sort(a, b) : online(a) - online(b) || sort(a, b);
      out.push({ key: "tailscale", label: "Tailscale", hosts: tailnet.sort(byOnline) });
    }
    return out;
  });

  const resultCount = $derived(sections.reduce((n, s) => n + s.hosts.length, 0));
  const collapsed = (key: string) => !searching && settings.isCollapsed(key);

  /** Keyboard-navigable items in visual order. */
  const navKeys = $derived(
    sections.flatMap((s) => [
      `section:${s.key}`,
      ...(collapsed(s.key) ? [] : s.hosts.map((h) => `host:${h.host.id}`)),
    ]),
  );
  const tabStop = $derived(navKey && navKeys.includes(navKey) ? navKey : (navKeys[0] ?? null));

  onMount(() => {
    ui.focusSearch = () => {
      searchEl?.focus();
      searchEl?.select();
    };
  });

  function focusNav(key: string | undefined) {
    if (!key) return;
    listEl?.querySelector<HTMLElement>(`[data-nav="${CSS.escape(key)}"]`)?.focus();
  }

  function onlistkeydown(e: KeyboardEvent) {
    const target = e.target as HTMLElement;
    const key = target.dataset?.nav;
    if (!key) return;
    const i = navKeys.indexOf(key);
    switch (e.key) {
      case "ArrowDown":
        focusNav(navKeys[i + 1]);
        break;
      case "ArrowUp":
        if (i === 0) searchEl?.focus();
        else focusNav(navKeys[i - 1]);
        break;
      case "Home":
        focusNav(navKeys[0]);
        break;
      case "End":
        focusNav(navKeys[navKeys.length - 1]);
        break;
      case "ArrowLeft":
        if (key.startsWith("section:")) {
          if (!searching) settings.toggleGroup(key.slice(8), true);
        } else {
          const section = sections.find((s) => s.hosts.some((h) => `host:${h.host.id}` === key));
          if (section) focusNav(`section:${section.key}`);
        }
        break;
      case "ArrowRight":
        if (key.startsWith("section:")) {
          if (collapsed(key.slice(8))) settings.toggleGroup(key.slice(8), false);
          else focusNav(navKeys[i + 1]);
        }
        break;
      default:
        if (e.key.length === 1 && /\S/.test(e.key) && !e.ctrlKey && !e.metaKey && !e.altKey) {
          // Type-to-search from the list.
          searchEl?.focus();
          return;
        }
        return;
    }
    e.preventDefault();
  }

  function onsearchkeydown(e: KeyboardEvent) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      focusNav(navKeys.find((k) => k.startsWith("host:")) ?? navKeys[0]);
    } else if (e.key === "Enter") {
      const first = sections[0]?.hosts[0]?.host;
      if (first) {
        e.preventDefault();
        openHost(first);
        if (searching) focusNav(`host:${first.id}`);
      }
    } else if (e.key === "Escape") {
      e.preventDefault();
      if (query) query = "";
      else searchEl?.blur();
    }
  }

  // ---------------------------------------------------------------- resize
  let resizing = $state(false);
  function onresizestart(e: PointerEvent) {
    e.preventDefault();
    resizing = true;
    const startX = e.clientX;
    const startW = settings.sidebarWidth;
    const target = e.currentTarget as HTMLElement;
    target.setPointerCapture(e.pointerId);
    const move = (ev: PointerEvent) => settings.setSidebarWidth(startW + ev.clientX - startX, false);
    const up = () => {
      resizing = false;
      settings.setSidebarWidth(settings.sidebarWidth);
      target.removeEventListener("pointermove", move);
      target.removeEventListener("pointerup", up);
    };
    target.addEventListener("pointermove", move);
    target.addEventListener("pointerup", up);
  }

  function onresizekey(e: KeyboardEvent) {
    if (e.key === "ArrowLeft") settings.setSidebarWidth(settings.sidebarWidth - 16);
    else if (e.key === "ArrowRight") settings.setSidebarWidth(settings.sidebarWidth + 16);
    else return;
    e.preventDefault();
  }

  const forwardsActive = $derived(tabs.active?.kind === "forwards" && tabs.active.hostId === null);
  const localActive = $derived((tabs.active?.kind === "files" || tabs.active?.kind === "terminal") && tabs.active.hostId === null);

  function localMenu(e: MouseEvent) {
    e.preventDefault();
    ui.openMenu(
      "This computer actions",
      [
        { label: "Open files", icon: FolderOpen, onSelect: () => openFiles(null, { reuse: true }) },
        { label: "New files tab", icon: FolderOpen, onSelect: () => openFiles(null) },
        { label: "Open in split view", icon: Columns2, onSelect: () => openFiles(null, { reuse: true, group: "beside" }) },
        { separator: true },
        { label: "Open local terminal", icon: SquareTerminal, onSelect: () => openTerminal(null) },
        { label: "Local terminal in split view", icon: Columns2, onSelect: () => openTerminal(null, { group: "beside" }) },
      ],
      e.clientX > 0 ? { x: e.clientX, y: e.clientY } : (e.currentTarget as HTMLElement),
    );
  }

  const connectedCount = $derived(hosts.list.filter((h) => connections.get(h.id).phase === "connected").length);
</script>

<aside class="sidebar" style:width="{settings.sidebarWidth}px" class:resizing aria-label="Hosts sidebar">
  <header class="head">
    <div class="search">
      <Search size={14} class="search-icon" />
      <input
        bind:this={searchEl}
        bind:value={query}
        type="text"
        placeholder="Search hosts"
        aria-label="Search hosts"
        aria-controls="host-list"
        autocomplete="off"
        spellcheck="false"
        onkeydown={onsearchkeydown}
      />
      {#if query}
        <button class="icon-btn sm clear" aria-label="Clear search" onclick={() => ((query = ""), searchEl?.focus())}>
          <X size={13} />
        </button>
      {:else}
        <kbd aria-hidden="true">/</kbd>
      {/if}
    </div>
    <button class="icon-btn" title="Add host ({modKey}+N)" aria-label="Add host" onclick={() => newHost()}>
      <Plus size={16} />
    </button>
  </header>

  <div class="local-row">
    <button
      type="button"
      class="local"
      class:selected={localActive}
      aria-current={localActive ? "page" : undefined}
      title="Browse files on this computer"
      onclick={() => openFiles(null, { reuse: true })}
      oncontextmenu={localMenu}
    >
      <span class="local-icon"><Laptop size={15} /></span>
      <span class="local-text">
        <span class="local-name">This computer</span>
        <span class="local-sub">Local files and shell</span>
      </span>
    </button>
    <button
      type="button"
      class="icon-btn sm local-term"
      title="Open local terminal"
      aria-label="Open local terminal"
      onclick={() => openTerminal(null)}
    >
      <SquareTerminal size={14} />
    </button>
  </div>

  <div class="local-row">
    <button
      type="button"
      class="local"
      class:selected={forwardsActive}
      aria-current={forwardsActive ? "page" : undefined}
      title="All port forwards ({modKey}+Shift+P)"
      onclick={() => tabs.openForwards(null)}
    >
      <span class="local-icon"><Cable size={15} /></span>
      <span class="local-text">
        <span class="local-name">Port forwards</span>
        <span class="local-sub">
          {#if forwards.troubleCount > 0}
            <span class="sub-warn">{forwards.troubleCount} need{forwards.troubleCount === 1 ? "s" : ""} attention</span>
          {:else if forwards.activeCount > 0}
            <span class="sub-ok">{forwards.activeCount} active</span> of {forwards.list.length}
          {:else if forwards.list.length > 0}
            {forwards.list.length} {forwards.list.length === 1 ? "rule" : "rules"}, none running
          {:else}
            Tunnels and SOCKS proxies
          {/if}
        </span>
      </span>
      {#if forwards.activeCount > 0}
        <span class="fwd-count" aria-hidden="true">{forwards.activeCount}</span>
      {/if}
    </button>
  </div>

  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <nav id="host-list" class="list" aria-label="Hosts" bind:this={listEl} onkeydown={onlistkeydown}>
    {#if !hosts.loaded}
      <div class="skeleton" aria-hidden="true">
        {#each [0, 1, 2, 3, 4] as i (i)}<div class="sk-row"><span></span><span></span></div>{/each}
      </div>
    {:else if hosts.list.length === 0}
      <div class="empty">
        <p class="empty-title">No hosts yet</p>
        <p class="empty-text">Hosts from <code>~/.ssh/config</code> appear here automatically.</p>
        <button class="btn btn-sm" onclick={() => newHost()}><Plus size={14} /> Add host</button>
      </div>
    {:else if resultCount === 0}
      <div class="empty">
        <p class="empty-title">No matches</p>
        <p class="empty-text">Nothing matches “{query.trim()}”.</p>
        <button class="btn btn-sm" onclick={() => (query = "")}>Clear search</button>
      </div>
    {:else}
      {#if searching}
        <div class="sr-only" role="status">{resultCount} {resultCount === 1 ? "host" : "hosts"} found</div>
      {/if}
      {#each sections as section (section.key)}
        {@const isCollapsed = collapsed(section.key)}
        {@const key = `section:${section.key}`}
        <section class="section">
          <h2 class="section-heading">
            <button
              type="button"
              class="section-toggle"
              data-nav={key}
              tabindex={tabStop === key ? 0 : -1}
              aria-expanded={!isCollapsed}
              aria-controls="section-{section.key}"
              onclick={() => !searching && settings.toggleGroup(section.key)}
              onfocus={() => (navKey = key)}
            >
              <span class="chev" class:open={!isCollapsed}><ChevronRight size={12} strokeWidth={2.5} /></span>
              <span class="section-label">{section.label}</span>
              <span class="count">{section.hosts.length}</span>
            </button>
          </h2>
          {#if !isCollapsed}
            <ul id="section-{section.key}" class="rows">
              {#each section.hosts as { host, highlight } (host.id)}
                <HostRow
                  {host}
                  {highlight}
                  navKey="host:{host.id}"
                  selected={tabs.activeHostId === host.id}
                  tabStop={tabStop === `host:${host.id}`}
                  onfocusrow={() => (navKey = `host:${host.id}`)}
                />
              {/each}
            </ul>
          {/if}
        </section>
      {/each}
    {/if}
  </nav>

  <footer class="footer" class:crowded={edits.list.length > 0}>
    <button class="btn btn-ghost btn-sm" onclick={() => (ui.settingsOpen = true)} title="Settings ({modKey}+,)" aria-label="Settings">
      <Settings size={14} /> <span class="settings-label">Settings</span>
    </button>
    <span class="spacer"></span>
    <EditIndicator />
    <TransferIndicator />
    {#if connectedCount > 0}
      <span class="connected" title="{connectedCount} connected">
        <span class="live-dot"></span>{connectedCount}
      </span>
    {/if}
    {#if hosts.warnings.length > 0}
      <button
        class="warn"
        onclick={() => (ui.warningsOpen = true)}
        title="ssh_config warnings"
        aria-label="{hosts.warnings.length} ssh_config {hosts.warnings.length === 1 ? 'warning' : 'warnings'}"
      >
        <TriangleAlert size={13} />
        {hosts.warnings.length}
      </button>
    {/if}
  </footer>

  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
  <div
    class="resizer"
    role="separator"
    aria-orientation="vertical"
    aria-label="Resize sidebar"
    aria-valuenow={settings.sidebarWidth}
    tabindex="0"
    onpointerdown={onresizestart}
    onkeydown={onresizekey}
    ondblclick={() => settings.setSidebarWidth(260)}
  ></div>
</aside>

<style>
  .sidebar {
    position: relative;
    container: sidebar / inline-size;
    display: flex;
    flex-direction: column;
    flex: none;
    height: 100%;
    min-width: 0;
    background: var(--bg-sidebar);
    border-right: 1px solid var(--border);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 6px;
    height: var(--tabstrip-h);
    padding: 0 8px 0 10px;
    flex: none;
  }
  .spacer {
    flex: 1;
  }

  .search {
    position: relative;
    display: flex;
    flex: 1;
    align-items: center;
    min-width: 0;
  }
  .search :global(.search-icon) {
    position: absolute;
    left: 9px;
    color: var(--text-subtle);
    pointer-events: none;
  }
  .search input {
    width: 100%;
    height: 30px;
    padding: 0 30px 0 29px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-input);
    font-size: var(--text-base);
    transition:
      border-color var(--dur-fast),
      box-shadow var(--dur-fast);
  }
  .search input::placeholder {
    color: var(--text-subtle);
  }
  .search input:focus {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .search kbd,
  .search .clear {
    position: absolute;
    right: 6px;
  }
  .search kbd {
    pointer-events: none;
  }

  .local-row {
    position: relative;
    margin: 0 8px 2px;
    flex: none;
  }
  .local-row + .local-row {
    margin-bottom: 8px;
  }
  .sub-ok {
    color: var(--success-text);
  }
  .sub-warn {
    color: var(--warning-text);
  }
  .fwd-count {
    display: inline-grid;
    place-items: center;
    min-width: 18px;
    height: 18px;
    margin-left: auto;
    padding: 0 5px;
    border-radius: var(--radius-full);
    background: var(--success-soft);
    color: var(--success-text);
    font-size: 10.5px;
    font-weight: var(--weight-semibold);
    font-variant-numeric: tabular-nums;
  }
  .local-term {
    position: absolute;
    right: 8px;
    top: 50%;
    transform: translateY(-50%);
    opacity: 0;
  }
  .local-row:hover .local-term,
  .local-term:focus-visible {
    opacity: 1;
  }
  .local {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    height: 40px;
    padding: 0 10px 0 8px;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text);
    text-align: left;
    flex: none;
    transition: background var(--dur-fast);
  }
  .local:hover {
    background: var(--bg-hover);
  }
  .local.selected {
    background: var(--bg-active);
  }
  .local:focus-visible {
    outline-offset: -2px;
  }
  .local-icon {
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    border-radius: var(--radius-sm);
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    color: var(--text-muted);
    flex: none;
  }
  .local-text {
    display: flex;
    flex-direction: column;
    min-width: 0;
    line-height: 1.25;
  }
  .local-name {
    font-weight: var(--weight-medium);
  }
  .local-sub {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--text-subtle);
    font-size: var(--text-sm);
  }
  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding-bottom: 8px;
  }
  .section + .section {
    margin-top: 6px;
  }
  .section-heading {
    margin: 0;
    font-size: inherit;
    font-weight: inherit;
  }
  .section-toggle {
    display: flex;
    align-items: center;
    gap: 4px;
    width: calc(100% - 16px);
    height: 26px;
    margin: 0 8px;
    padding: 0 8px 0 4px;
    border: 0;
    border-radius: var(--radius-xs);
    background: transparent;
    color: var(--text-subtle);
    font-size: var(--text-xs);
    font-weight: var(--weight-semibold);
    letter-spacing: 0.04em;
    text-transform: uppercase;
    text-align: left;
  }
  .section-toggle:hover {
    color: var(--text-muted);
  }
  .section-toggle:focus-visible {
    outline-offset: -2px;
  }
  .chev {
    display: flex;
    transition: transform var(--dur-fast) var(--ease-out);
  }
  .chev.open {
    transform: rotate(90deg);
  }
  .section-label {
    flex: 1;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .count {
    font-weight: var(--weight-medium);
    letter-spacing: 0;
    opacity: 0.8;
  }
  .rows {
    margin: 0;
    padding: 0;
  }

  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
    padding: 32px 20px;
    text-align: center;
  }
  .empty-title {
    font-weight: var(--weight-medium);
  }
  .empty-text {
    margin-bottom: 8px;
    color: var(--text-muted);
    font-size: var(--text-sm);
    overflow-wrap: anywhere;
  }

  .skeleton {
    padding: 8px 18px;
  }
  .sk-row {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 9px 0;
  }
  .sk-row span {
    height: 9px;
    width: 60%;
    border-radius: 4px;
    background: var(--bg-active);
  }
  .sk-row span + span {
    width: 85%;
    opacity: 0.6;
  }

  .footer {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: none;
    height: 44px;
    padding: 0 8px;
    border-top: 1px solid var(--border);
  }
  .footer .btn {
    color: var(--text-muted);
  }
  /* Narrow sidebar: the settings button keeps only its icon. */
  @container sidebar (max-width: 239px) {
    .settings-label {
      display: none;
    }
  }
  @container sidebar (max-width: 299px) {
    .crowded .settings-label {
      display: none;
    }
  }
  .footer .btn:hover {
    color: var(--text);
  }
  .connected {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 0 6px;
    color: var(--text-muted);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
  }
  .live-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--success);
  }
  .warn {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 24px;
    padding: 0 8px;
    border: 0;
    border-radius: var(--radius-full);
    background: var(--warning-soft);
    color: var(--warning-text);
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
  }
  .warn:hover {
    filter: brightness(0.97);
  }

  .resizer {
    position: absolute;
    top: 0;
    right: -3px;
    bottom: 0;
    width: 6px;
    cursor: col-resize;
    z-index: 5;
  }
  .resizer:hover,
  .resizing .resizer,
  .resizer:focus-visible {
    outline: none;
    background: linear-gradient(to right, transparent 2px, var(--accent) 2px, var(--accent) 4px, transparent 4px);
    opacity: 0.6;
  }
  .resizing {
    user-select: none;
  }
</style>
