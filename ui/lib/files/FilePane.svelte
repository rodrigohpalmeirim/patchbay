<!--
  One file browser pane: toolbar (history, path bar, filter, actions),
  search bar, reconnect banner, virtualized list, preview panel and status bar.
-->
<script lang="ts">
  import ArrowLeft from "@lucide/svelte/icons/arrow-left";
  import ArrowRight from "@lucide/svelte/icons/arrow-right";
  import ArrowUp from "@lucide/svelte/icons/arrow-up";
  import Columns2 from "@lucide/svelte/icons/columns-2";
  import Ellipsis from "@lucide/svelte/icons/ellipsis";
  import Eye from "@lucide/svelte/icons/eye";
  import EyeOff from "@lucide/svelte/icons/eye-off";
  import FilePlus from "@lucide/svelte/icons/file-plus";
  import FolderPlus from "@lucide/svelte/icons/folder-plus";
  import FolderUp from "@lucide/svelte/icons/folder-up";
  import Laptop from "@lucide/svelte/icons/laptop";
  import ListFilter from "@lucide/svelte/icons/list-filter";
  import LoaderCircle from "@lucide/svelte/icons/loader-circle";
  import PanelRight from "@lucide/svelte/icons/panel-right";
  import RefreshCw from "@lucide/svelte/icons/refresh-cw";
  import Search from "@lucide/svelte/icons/search";
  import Server from "@lucide/svelte/icons/server";
  import Square from "@lucide/svelte/icons/square";
  import Copy from "@lucide/svelte/icons/copy";
  import Upload from "@lucide/svelte/icons/upload";
  import WifiOff from "@lucide/svelte/icons/wifi-off";
  import X from "@lucide/svelte/icons/x";
  import ArrowDownUp from "@lucide/svelte/icons/arrow-down-up";
  import { tick, untrack } from "svelte";
  import FileList from "./FileList.svelte";
  import PathBar from "./PathBar.svelte";
  import PreviewPanel from "./PreviewPanel.svelte";
  import * as ops from "./ops";
  import { connections } from "../stores/connections.svelte";
  import { dnd, dropIsMove } from "../stores/dnd.svelte";
  import { filePrefs, files, type FileBrowser, type SortKey } from "../stores/files.svelte";
  import { hosts } from "../stores/hosts.svelte";
  import { tabs, type FilesTab } from "../stores/tabs.svelte";
  import { ui, type MenuItem } from "../stores/ui.svelte";
  import type { Host } from "../types";
  import { isDirLike } from "../util/fileIcons";
  import { formatBytes, plural } from "../util/format";
  import { basename } from "../util/paths";
  import { hasMod, modKey } from "../util/platform";

  let {
    browser: b,
    host,
    active,
    connected,
    stageLabel,
    onreconnect,
  }: {
    browser: FileBrowser;
    host: Host | null;
    active: boolean;
    connected: boolean;
    stageLabel: string;
    onreconnect: () => void;
  } = $props();

  let width = $state(900);
  const compact = $derived(width < 760);
  const narrow = $derived(width < 480);
  /** Very small panes keep only the essentials; the rest lives in "More". */
  const tiny = $derived(width < 330);
  const overlayPreview = $derived(width < 640);

  let pathBar = $state<ReturnType<typeof PathBar>>();
  let editingPath = $state(false);
  let filterInput = $state<HTMLInputElement>();
  let searchInput = $state<HTMLInputElement>();
  let query = $state("");

  const tab = $derived(tabs.get(b.tabId) as FilesTab | undefined);
  const remote = $derived(b.hostId !== null);

  // ------------------------------------------------------------ search

  let debounce: ReturnType<typeof setTimeout> | undefined;
  function oninput() {
    clearTimeout(debounce);
    const q = query.trim();
    if (q.length < 2) return;
    debounce = setTimeout(() => void b.startSearch(q), 350);
  }

  function closeSearch() {
    clearTimeout(debounce);
    query = "";
    b.cancelSearch();
    b.focusTick++;
  }

  function onsearchkey(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === "Enter") {
      e.preventDefault();
      clearTimeout(debounce);
      if (query.trim() && query.trim() !== b.search?.query) void b.startSearch(query);
      else if (b.rows.length) {
        b.selectOnly(b.rows[0].path);
        b.focusTick++;
      }
    } else if (e.key === "Escape") {
      e.preventDefault();
      closeSearch();
    } else if (e.key === "ArrowDown" && b.rows.length) {
      e.preventDefault();
      b.selectOnly(b.rows[0].path);
      b.focusTick++;
    }
  }

  $effect(() => {
    if (b.searchOpen) {
      void tick().then(() => {
        searchInput?.focus();
        searchInput?.select();
      });
    }
  });

  // Leaving the tab stops a running search.
  $effect(() => {
    if (!active) untrack(() => b.search?.running && b.stopSearch());
  });

  // ------------------------------------------------------------ filter

  function onfilterkey(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === "Escape") {
      e.preventDefault();
      if (b.filter) b.filter = "";
      else b.filterOpen = false;
      b.focusTick++;
    } else if (e.key === "ArrowDown" || e.key === "Enter") {
      e.preventDefault();
      if (b.rows.length) b.selectOnly(b.rows[0].path);
      b.focusTick++;
    }
  }

  async function openFilter() {
    b.filterOpen = true;
    await tick();
    filterInput?.focus();
  }

  // ------------------------------------------------------------ toggles

  function toggleHidden() {
    filePrefs.showHidden = !filePrefs.showHidden;
    filePrefs.save();
  }

  function togglePreview() {
    ops.setPreview(b, !b.previewOpen);
  }

  // -------------------------------------------------------------- menus

  function besideItems(): MenuItem[] {
    // "beside": a neighbouring group, created on the right if not split yet.
    const items: MenuItem[] = [];
    items.push({
      label: "This computer",
      icon: Laptop,
      onSelect: () => ops.openFiles(null, { reuse: b.hostId !== null, group: "beside" }),
    });
    for (const h of hosts.list) {
      if (connections.get(h.id).phase !== "connected" || h.id === b.hostId) continue;
      items.push({ label: h.name, icon: Server, onSelect: () => ops.openFiles(h.id, { reuse: true, group: "beside" }) });
    }
    if (tab) {
      items.push({ separator: true });
      items.push({ label: "Duplicate this tab", icon: Copy, onSelect: () => tab && ops.duplicateFilesTab(tab, "beside") });
    }
    return items;
  }

  function openBeside(e: MouseEvent) {
    ui.openMenu("Open beside", besideItems(), e.currentTarget as HTMLElement);
  }

  const SORTS: [SortKey, string][] = [
    ["name", "Name"],
    ["size", "Size"],
    ["modified", "Modified"],
    ["permissions", "Permissions"],
    ["owner", "Owner"],
  ];

  function moreMenu(e: MouseEvent) {
    const items: MenuItem[] = [];
    if (tiny) {
      items.push({ label: "Forward", icon: ArrowRight, hint: "Alt+→", disabled: !b.canForward, onSelect: () => b.forward() });
      items.push({ separator: true });
    }
    items.push(
      { label: "New folder", icon: FolderPlus, hint: `${modKey}+Shift+N`, disabled: !!b.search, onSelect: () => ops.startCreate(b, "dir") },
      { label: "New file", icon: FilePlus, disabled: !!b.search, onSelect: () => ops.startCreate(b, "file") },
    );
    if (remote) {
      items.push({ label: "Upload files…", icon: Upload, onSelect: () => void ops.upload(b) });
      items.push({ label: "Upload folder…", icon: FolderUp, onSelect: () => void ops.upload(b, true) });
    }
    items.push({ separator: true });
    items.push({ label: "Search in this folder", icon: Search, hint: `${modKey}+F`, onSelect: () => b.openSearch() });
    items.push({ label: filePrefs.showHidden ? "Hide hidden files" : "Show hidden files", icon: filePrefs.showHidden ? EyeOff : Eye, hint: `${modKey}+Shift+.`, onSelect: toggleHidden });
    items.push({ label: b.previewOpen ? "Hide preview" : "Show preview", icon: PanelRight, hint: "Space", onSelect: togglePreview });
    items.push({ separator: true });
    for (const [key, label] of SORTS) {
      const on = filePrefs.sortKey === key;
      items.push({
        label: `Sort by ${label.toLowerCase()}${on ? (filePrefs.sortDir === "asc" ? "  ↑" : "  ↓") : ""}`,
        icon: on ? ArrowDownUp : undefined,
        onSelect: () => {
          if (on) filePrefs.sortDir = filePrefs.sortDir === "asc" ? "desc" : "asc";
          else {
            filePrefs.sortKey = key;
            filePrefs.sortDir = key === "size" || key === "modified" ? "desc" : "asc";
          }
          filePrefs.save();
        },
      });
    }
    items.push(ops.terminalHereItem(b));
    items.push({ separator: true });
    items.push(...besideItems().map((i) => ("label" in i ? { ...i, label: i.label === "Duplicate this tab" ? "Duplicate tab beside" : `Open ${i.label} beside` } : i)));
    items.push({ separator: true });
    items.push({ label: "Refresh", icon: RefreshCw, hint: "F5", onSelect: () => void b.refresh() });
    ui.openMenu("View options", items, e.currentTarget as HTMLElement);
  }

  // ----------------------------------------------------------- keyboard

  function onkeydown(e: KeyboardEvent) {
    const mod = hasMod(e);
    const k = e.key.toLowerCase();
    if (mod && k === "f") {
      b.openSearch();
      if (b.searchOpen) void tick().then(() => searchInput?.focus());
    } else if (mod && k === "l") {
      void pathBar?.edit();
    } else if (e.key === "F5" || (mod && k === "r")) {
      void b.refresh();
    } else if ((e.altKey && e.key === "ArrowLeft") || (mod && e.key === "[")) {
      b.back();
    } else if ((e.altKey && e.key === "ArrowRight") || (mod && e.key === "]")) {
      b.forward();
    } else if (e.altKey && e.key === "ArrowUp") {
      b.up();
    } else if (mod && e.shiftKey && k === "n") {
      ops.startCreate(b, "dir");
    } else if (mod && e.shiftKey && (e.key === "." || e.key === ">")) {
      toggleHidden();
    } else return;
    e.preventDefault();
    e.stopPropagation();
  }

  // ------------------------------------------------------------- drops

  const fileDrop = $derived(dnd.file?.target?.paneId === b.tabId ? dnd.file : null);
  const osDrop = $derived(dnd.os?.target?.paneId === b.tabId ? dnd.os : null);
  const dropTarget = $derived(fileDrop?.target ?? osDrop?.target ?? null);
  const dropLabel = $derived.by(() => {
    if (!dropTarget) return "";
    const into = basename(dropTarget.dir);
    if (osDrop) return `${osDrop.own ? "Copy" : "Upload"} ${osDrop.count ? plural(osDrop.count, "item") : "files"} to ${into}`;
    if (fileDrop) return `${dropIsMove(fileDrop) ? "Move" : "Copy"} ${plural(fileDrop.entries.length, "item")} to ${into}`;
    return "";
  });

  // ----------------------------------------------------------- status

  const status = $derived.by(() => {
    const sel = b.selectedEntries;
    if (sel.length > 0) {
      const files = sel.filter((e) => !isDirLike(e));
      const size = files.reduce((n, e) => n + e.size, 0);
      return `${plural(sel.length, "item")} selected${files.length ? ` · ${formatBytes(size)}` : ""}`;
    }
    if (b.search) return `${plural(b.rows.length, "result")}`;
    return b.listing ? plural(b.rows.length, "item") : "";
  });

  // Preview resize
  let resizing = $state(false);
  function onresizestart(e: PointerEvent) {
    e.preventDefault();
    resizing = true;
    const startX = e.clientX;
    const startW = filePrefs.previewWidth;
    const target = e.currentTarget as HTMLElement;
    target.setPointerCapture(e.pointerId);
    const move = (ev: PointerEvent) => {
      filePrefs.previewWidth = Math.round(Math.min(Math.max(220, startW - (ev.clientX - startX)), Math.max(240, width * 0.6)));
    };
    const up = () => {
      resizing = false;
      filePrefs.save();
      target.removeEventListener("pointermove", move);
      target.removeEventListener("pointerup", up);
    };
    target.addEventListener("pointermove", move);
    target.addEventListener("pointerup", up);
  }
</script>

{#snippet navButtons()}
  <div class="group">
    <button class="icon-btn" aria-label="Back" title="Back (Alt+←)" disabled={!b.canBack} onclick={() => b.back()}><ArrowLeft size={15} /></button>
    {#if !tiny}
      <button class="icon-btn" aria-label="Forward" title="Forward (Alt+→)" disabled={!b.canForward} onclick={() => b.forward()}><ArrowRight size={15} /></button>
    {/if}
    <button class="icon-btn" aria-label="Up one level" title="Up (Alt+↑)" disabled={b.parent == null} onclick={() => b.up()}><ArrowUp size={15} /></button>
    <button class="icon-btn" aria-label="Refresh" title="Refresh (F5)" disabled={b.path == null || (remote && !connected)} onclick={() => b.refresh()}>
      <RefreshCw size={14} class={b.loading ? "spin" : ""} />
    </button>
  </div>
{/snippet}

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="pane"
  class:drop-active={!!dropTarget && !dropTarget.rowPath}
  class:resizing
  data-pane={b.tabId}
  bind:clientWidth={width}
  {onkeydown}
>
  <div class="toolbar" class:narrow>
    {@render navButtons()}
    {#if !narrow}
      <PathBar browser={b} bind:this={pathBar} bind:editing={editingPath} />
    {:else}
      <span class="spacer"></span>
    {/if}
    {#if !compact}
      <div class="filter" class:active={!!b.filter}>
        <ListFilter size={13} class="filter-icon" />
        <input
          bind:this={filterInput}
          bind:value={b.filter}
          placeholder="Filter"
          aria-label="Filter this folder"
          spellcheck="false"
          autocomplete="off"
          onkeydown={onfilterkey}
        />
        {#if b.filter}
          <button class="icon-btn sm clear" aria-label="Clear filter" onclick={() => (b.filter = "")}><X size={12} /></button>
        {/if}
      </div>
    {/if}
    <div class="group">
      {#if compact}
        <button class="icon-btn" class:on={b.filterOpen || !!b.filter} aria-label="Filter" aria-pressed={b.filterOpen} title="Filter" onclick={() => (b.filterOpen ? ((b.filterOpen = false), (b.filter = "")) : openFilter())}>
          <ListFilter size={15} />
        </button>
      {/if}
      {#if !tiny}
        <button class="icon-btn" aria-label="Search" title="Search in this folder ({modKey}+F)" onclick={() => b.openSearch()} disabled={!b.path}>
          <Search size={15} />
        </button>
      {/if}
      {#if !narrow}
      <button
        class="icon-btn"
        class:on={filePrefs.showHidden}
        aria-label="Show hidden files"
        aria-pressed={filePrefs.showHidden}
        title={filePrefs.showHidden ? "Hide hidden files" : "Show hidden files"}
        onclick={toggleHidden}
      >
        {#if filePrefs.showHidden}<Eye size={15} />{:else}<EyeOff size={15} />{/if}
      </button>
      {/if}
      {#if !compact}
        <button class="icon-btn" aria-label="New folder" title="New folder ({modKey}+Shift+N)" disabled={!b.listing || !!b.search} onclick={() => ops.startCreate(b, "dir")}>
          <FolderPlus size={15} />
        </button>
        {#if remote}
          <button class="icon-btn" aria-label="Upload files" title="Upload files…" disabled={!b.listing} onclick={() => ops.upload(b)}>
            <Upload size={15} />
          </button>
        {/if}
        <button class="icon-btn" aria-label="Open beside" aria-haspopup="menu" title="Open beside (split view)" onclick={openBeside}>
          <Columns2 size={15} />
        </button>
      {/if}
      {#if !narrow}
        <button class="icon-btn" class:on={b.previewOpen} aria-label="Preview panel" aria-pressed={b.previewOpen} title="Preview (Space)" onclick={togglePreview}>
          <PanelRight size={15} />
        </button>
      {/if}
      <button class="icon-btn" aria-label="View options" aria-haspopup="menu" title="More" onclick={moreMenu}>
        <Ellipsis size={15} />
      </button>
    </div>
  </div>

  {#if narrow}
    <div class="row2">
      <PathBar browser={b} bind:this={pathBar} bind:editing={editingPath} />
    </div>
  {/if}

  {#if compact && b.filterOpen}
    <div class="row2">
      <div class="filter wide" class:active={!!b.filter}>
        <ListFilter size={13} class="filter-icon" />
        <input
          bind:this={filterInput}
          bind:value={b.filter}
          placeholder="Filter this folder"
          aria-label="Filter this folder"
          spellcheck="false"
          autocomplete="off"
          onkeydown={onfilterkey}
        />
      </div>
    </div>
  {/if}

  {#if b.loading && b.listing}
    <div class="loadbar" aria-hidden="true"></div>
  {/if}

  {#if remote && !connected}
    <div class="banner" role="status">
      {#if stageLabel}
        <LoaderCircle size={14} class="spin" />
        <span class="banner-text">{stageLabel}</span>
      {:else}
        <WifiOff size={14} />
        <span class="banner-text"><strong>Disconnected from {host?.name}.</strong> Showing the last loaded listing.</span>
        <button class="btn btn-sm" onclick={onreconnect}>Reconnect</button>
      {/if}
    </div>
  {/if}

  {#if b.searchOpen}
    <div class="searchbar" role="search">
      <Search size={14} class="sb-icon" />
      <input
        bind:this={searchInput}
        bind:value={query}
        placeholder="Search in {b.path ? basename(b.path) : 'folder'} — names, * and ? wildcards"
        aria-label="Search file names"
        spellcheck="false"
        autocomplete="off"
        {oninput}
        onkeydown={onsearchkey}
      />
      {#if b.search}
        <span class="sb-status" aria-live="polite">
          {#if b.search.running}
            <LoaderCircle size={13} class="spin" /> {b.search.results.length.toLocaleString()} found
          {:else if b.search.error}
            <span class="err">{b.search.error}</span>
          {:else}
            {plural(b.search.results.length, "result")}
          {/if}
        </span>
        {#if b.search.running}
          <button class="btn btn-sm" onclick={() => b.stopSearch()}><Square size={11} /> Stop</button>
        {/if}
      {/if}
      <button class="icon-btn sm" aria-label="Close search" title="Close search (Esc)" onclick={closeSearch}><X size={14} /></button>
    </div>
  {/if}

  <div class="body" class:overlay={overlayPreview}>
    <FileList browser={b} />
    {#if b.previewOpen && b.listing}
      {#if !overlayPreview}
        <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
        <div class="p-resizer" role="separator" aria-orientation="vertical" aria-label="Resize preview" onpointerdown={onresizestart}></div>
      {/if}
      <div class="preview-wrap" style:width={overlayPreview ? undefined : `${filePrefs.previewWidth}px`}>
        <PreviewPanel browser={b} onclose={() => ops.setPreview(b, false)} />
      </div>
    {/if}
    {#if dropTarget}
      <div class="drop-label" class:row-target={!!dropTarget.rowPath}>{dropLabel}</div>
    {/if}
  </div>

  <footer class="status">
    <span class="st-main">{status}</span>
    {#if b.hiddenCount > 0 && !b.selectedEntries.length}
      <span class="st-dim">· {b.hiddenCount} hidden</span>
    {/if}
    {#if b.filter && !b.search}
      <span class="st-chip">Filtered</span>
    {/if}
    <span class="spacer"></span>
    {#if files.clipboard}
      <span class="st-dim" title="Paste with {modKey}+V">{plural(files.clipboard.entries.length, "item")} on clipboard</span>
    {/if}
  </footer>
</div>


<style>
  .pane {
    position: relative;
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    min-width: 0;
    background: var(--bg);
  }
  .pane.drop-active::after {
    content: "";
    position: absolute;
    inset: 2px;
    border: 2px solid var(--accent);
    border-radius: var(--radius-md);
    background: var(--accent-soft);
    pointer-events: none;
    z-index: 5;
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 44px;
    padding: 0 8px;
    flex: none;
  }
  .group {
    display: flex;
    align-items: center;
    gap: 1px;
    flex: none;
  }
  .toolbar .icon-btn {
    width: 28px;
    height: 28px;
  }
  .icon-btn.on {
    background: var(--bg-active);
    color: var(--text);
  }
  .spacer {
    flex: 1;
  }
  .row2 {
    display: flex;
    padding: 0 8px 8px;
    flex: none;
  }
  .filter {
    position: relative;
    display: flex;
    align-items: center;
    flex: 0 1 170px;
    min-width: 110px;
  }
  .filter.wide {
    flex: 1;
  }
  .filter :global(.filter-icon) {
    position: absolute;
    left: 8px;
    color: var(--text-subtle);
    pointer-events: none;
  }
  .filter input {
    width: 100%;
    height: 28px;
    padding: 0 24px 0 27px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-input);
    font-size: var(--text-base);
  }
  .filter input::placeholder {
    color: var(--text-subtle);
  }
  .filter input:focus {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .filter.active input {
    border-color: var(--accent);
  }
  .filter .clear {
    position: absolute;
    right: 3px;
  }
  .loadbar {
    position: relative;
    height: 2px;
    margin-top: -2px;
    overflow: hidden;
    flex: none;
  }
  .loadbar::after {
    content: "";
    position: absolute;
    top: 0;
    bottom: 0;
    width: 30%;
    background: var(--accent);
    border-radius: 2px;
    animation: slide 1s var(--ease-out) infinite;
  }
  @keyframes slide {
    from {
      left: -30%;
    }
    to {
      left: 100%;
    }
  }
  .banner {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0 8px 8px;
    padding: 6px 6px 6px 10px;
    min-height: 34px;
    border-radius: var(--radius-sm);
    background: var(--warning-soft);
    color: var(--warning-text);
    font-size: var(--text-sm);
    flex: none;
  }
  .banner-text {
    flex: 1;
    min-width: 0;
    color: var(--text);
  }
  .banner strong {
    font-weight: var(--weight-semibold);
  }
  .searchbar {
    position: relative;
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0 8px 8px;
    padding: 0 4px 0 10px;
    height: 34px;
    border: 1px solid var(--accent);
    border-radius: var(--radius-sm);
    background: var(--bg-input);
    box-shadow: 0 0 0 3px var(--accent-soft);
    flex: none;
  }
  .searchbar :global(.sb-icon) {
    color: var(--accent-text);
    flex: none;
  }
  .searchbar input {
    flex: 1;
    min-width: 60px;
    height: 100%;
    border: 0;
    background: transparent;
    font-size: var(--text-base);
  }
  .searchbar input:focus,
  .searchbar input:focus-visible {
    outline: none;
  }
  .searchbar input::placeholder {
    color: var(--text-subtle);
  }
  .sb-status {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    flex: none;
    color: var(--text-muted);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .sb-status .err {
    color: var(--danger-text);
    max-width: 200px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .body {
    position: relative;
    display: flex;
    flex: 1;
    min-height: 0;
    border-top: 1px solid var(--border);
  }
  .p-resizer {
    position: relative;
    flex: none;
    width: 1px;
    background: var(--border);
    cursor: col-resize;
  }
  .p-resizer::before {
    content: "";
    position: absolute;
    inset: 0 -3px;
  }
  .p-resizer:hover,
  .resizing .p-resizer {
    background: var(--accent);
  }
  .preview-wrap {
    flex: none;
    min-width: 0;
    height: 100%;
  }
  .overlay .preview-wrap {
    position: absolute;
    top: 0;
    right: 0;
    bottom: 0;
    width: min(320px, 92%);
    border-left: 1px solid var(--border);
    box-shadow: var(--shadow-lg);
    z-index: 4;
  }
  .drop-label {
    position: absolute;
    left: 50%;
    bottom: 14px;
    transform: translateX(-50%);
    max-width: calc(100% - 32px);
    padding: 6px 12px;
    border-radius: var(--radius-full);
    background: var(--accent);
    color: var(--text-on-accent);
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    box-shadow: var(--shadow-md);
    pointer-events: none;
    z-index: 6;
  }
  .status {
    display: flex;
    align-items: center;
    gap: 6px;
    height: 26px;
    padding: 0 12px;
    border-top: 1px solid var(--border-subtle);
    color: var(--text-muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    overflow: hidden;
    flex: none;
  }
  .st-dim {
    color: var(--text-subtle);
  }
  .st-chip {
    padding: 0 6px;
    border-radius: var(--radius-full);
    background: var(--accent-soft);
    color: var(--accent-text);
    font-weight: var(--weight-medium);
  }
</style>
