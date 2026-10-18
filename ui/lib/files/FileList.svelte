<!--
  Virtualized file grid. Only the rows in (and a little around) the viewport
  are rendered, absolutely positioned inside a spacer as tall as the whole
  list, so directories with thousands of entries scroll smoothly.

  Keyboard (grid focused): arrows/Home/End/PageUp/PageDown move (Shift
  extends), Enter opens, Backspace / Alt+Up goes up, Space toggles the
  preview, F2 renames, Delete deletes, Ctrl/Cmd+A/C/V select all/copy/paste,
  Ctrl/Cmd+Space toggles selection, typing jumps to a name, Esc clears.
-->
<script lang="ts">
  import ArrowDown from "@lucide/svelte/icons/arrow-down";
  import ArrowUp from "@lucide/svelte/icons/arrow-up";
  import CircleAlert from "@lucide/svelte/icons/circle-alert";
  import FolderOpen from "@lucide/svelte/icons/folder-open";
  import FolderPlus from "@lucide/svelte/icons/folder-plus";
  import FolderX from "@lucide/svelte/icons/folder-x";
  import Link2 from "@lucide/svelte/icons/link-2";
  import LoaderCircle from "@lucide/svelte/icons/loader-circle";
  import Lock from "@lucide/svelte/icons/lock";
  import SearchX from "@lucide/svelte/icons/search-x";
  import Folder from "@lucide/svelte/icons/folder";
  import File from "@lucide/svelte/icons/file";
  import { tick } from "svelte";
  import * as ops from "./ops";
  import { dnd } from "../stores/dnd.svelte";
  import { filePrefs, type FileBrowser, type SortKey } from "../stores/files.svelte";
  import type { FileEntry } from "../types";
  import { iconFor, isDirLike } from "../util/fileIcons";
  import { formatBytes, formatExact, formatModified, octal, permString } from "../util/format";
  import { dirname, relative } from "../util/paths";
  import { hasMod, isMac } from "../util/platform";

  let { browser: b }: { browser: FileBrowser } = $props();

  const RH = 28;
  const HEAD = 30;
  const OVERSCAN = 10;

  let grid = $state<HTMLElement>();
  let scrollTop = $state(0);
  let viewH = $state(400);
  let width = $state(800);
  let now = $state(Date.now());

  $effect(() => {
    const t = setInterval(() => (now = Date.now()), 30_000);
    return () => clearInterval(t);
  });

  // ------------------------------------------------------------- columns

  interface Col {
    key: SortKey | "folder";
    label: string;
    size: string;
    align?: "end";
  }

  const cols = $derived.by((): Col[] => {
    const out: Col[] = [{ key: "name", label: "Name", size: "minmax(140px, 1fr)" }];
    if (b.search && width >= 520) out.push({ key: "folder", label: "Folder", size: "minmax(100px, 0.75fr)" });
    if (width >= 300) out.push({ key: "size", label: "Size", size: "84px", align: "end" });
    if (width >= 460) out.push({ key: "modified", label: "Modified", size: "148px" });
    if (width >= 660) out.push({ key: "permissions", label: "Permissions", size: "104px" });
    if (width >= 800) out.push({ key: "owner", label: "Owner", size: "128px" });
    return out;
  });
  const template = $derived(cols.map((c) => c.size).join(" "));
  const has = (key: Col["key"]) => cols.some((c) => c.key === key);

  function sortBy(key: SortKey) {
    if (filePrefs.sortKey === key) filePrefs.sortDir = filePrefs.sortDir === "asc" ? "desc" : "asc";
    else {
      filePrefs.sortKey = key;
      filePrefs.sortDir = key === "modified" || key === "size" ? "desc" : "asc";
    }
    filePrefs.save();
  }

  // ---------------------------------------------------------- windowing

  const rows = $derived(b.rows);
  const pre = $derived(b.creating ? RH : 0);
  const first = $derived(Math.max(0, Math.floor((scrollTop - HEAD - pre) / RH) - OVERSCAN));
  const last = $derived(Math.min(rows.length, Math.ceil((scrollTop - HEAD - pre + viewH) / RH) + OVERSCAN));
  const windowRows = $derived(rows.slice(first, last).map((e, k) => ({ e, i: first + k })));

  const cursorIndex = $derived(b.cursor ? (b.rowIndex.get(b.cursor) ?? -1) : -1);
  const rowId = (i: number) => `row-${b.tabId}-${i}`;

  function ensureVisible(i: number) {
    if (!grid || i < 0) return;
    const top = HEAD + pre + i * RH;
    const view = grid.clientHeight;
    if (top - HEAD < grid.scrollTop) grid.scrollTop = top - HEAD;
    else if (top + RH > grid.scrollTop + view) grid.scrollTop = top + RH - view;
  }

  // Requests from outside (navigate with selection, created entries, …).
  $effect(() => {
    void b.revealTick;
    void tick().then(() => ensureVisible(b.cursor ? (b.rowIndex.get(b.cursor) ?? -1) : -1));
  });
  $effect(() => {
    void b.focusTick;
    void tick().then(() => grid?.focus({ preventScroll: true }));
  });
  // New directory → back to the top.
  $effect(() => {
    void b.path;
    void b.search?.query;
    if (grid) grid.scrollTop = 0;
  });
  $effect(() => {
    if (b.creating && grid) grid.scrollTop = 0;
  });

  // ---------------------------------------------------------- selection

  function rowAt(target: EventTarget | null): { e: FileEntry; i: number } | null {
    const el = (target as HTMLElement | null)?.closest<HTMLElement>("[data-index]");
    if (!el) return null;
    const i = Number(el.dataset.index);
    const e = rows[i];
    return e ? { e, i } : null;
  }

  function onclick(ev: MouseEvent) {
    if (dnd.justDropped) return;
    const hit = rowAt(ev.target);
    if (!hit) {
      if (!hasMod(ev) && !ev.shiftKey) b.clearSelection();
      return;
    }
    if (b.renaming === hit.e.path) return;
    if (ev.shiftKey) b.selectRange(hit.e.path, hasMod(ev));
    else if (hasMod(ev)) b.toggle(hit.e.path);
    else b.selectOnly(hit.e.path);
  }

  function ondblclick(ev: MouseEvent) {
    const hit = rowAt(ev.target);
    if (hit && b.renaming !== hit.e.path) ops.open(b, hit.e);
  }

  function onpointerdown(ev: PointerEvent) {
    if (ev.button !== 0) return;
    const hit = rowAt(ev.target);
    if (!hit || b.renaming === hit.e.path) return;
    if (!b.selected.has(hit.e.path) && !hasMod(ev) && !ev.shiftKey) b.selectOnly(hit.e.path);
    dnd.armFileDrag(
      ev,
      b.tabId,
      b.hostId,
      () => (b.selected.has(hit.e.path) ? b.selectedEntries : [hit.e]),
      (drag) => void ops.drop(drag),
    );
  }

  function oncontextmenu(ev: MouseEvent) {
    ev.preventDefault();
    const hit = rowAt(ev.target);
    if (hit && !b.selected.has(hit.e.path)) b.selectOnly(hit.e.path);
    ops.showMenu(b, hit ? b.selectedEntries : [], { x: ev.clientX, y: ev.clientY });
  }

  // ----------------------------------------------------------- keyboard

  let typed = "";
  let typedAt = 0;

  function moveTo(i: number, extend: boolean, keepSelection = false) {
    if (!rows.length) return;
    const j = Math.max(0, Math.min(rows.length - 1, i));
    const path = rows[j].path;
    if (extend) b.selectRange(path);
    else if (keepSelection) b.cursor = path;
    else b.selectOnly(path);
    ensureVisible(j);
  }

  function typeAhead(ch: string) {
    const t = performance.now();
    typed = t - typedAt > 800 ? ch : typed + ch;
    typedAt = t;
    const q = typed.toLowerCase();
    const start = typed.length === 1 ? cursorIndex + 1 : Math.max(0, cursorIndex);
    for (let k = 0; k < rows.length; k++) {
      const i = (start + k) % rows.length;
      if (rows[i].name.toLowerCase().startsWith(q)) {
        moveTo(i, false);
        return;
      }
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.target !== grid) return;
    const mod = hasMod(e);
    const page = Math.max(1, Math.floor((viewH - HEAD) / RH) - 1);
    const cur = cursorIndex;
    const sel = b.selectedEntries;
    const key = e.key;
    switch (true) {
      case key === "ArrowDown" && !e.altKey:
        moveTo(cur + 1, e.shiftKey, mod);
        break;
      case key === "ArrowUp" && e.altKey:
      case key === "Backspace" && !mod && !b.search:
        b.up();
        break;
      case key === "ArrowUp":
        moveTo(cur === -1 ? rows.length - 1 : cur - 1, e.shiftKey, mod);
        break;
      case key === "Home":
        moveTo(0, e.shiftKey);
        break;
      case key === "End":
        moveTo(rows.length - 1, e.shiftKey);
        break;
      case key === "PageDown":
        moveTo(cur + page, e.shiftKey);
        break;
      case key === "PageUp":
        moveTo(cur - page, e.shiftKey);
        break;
      case key === "ArrowDown" && e.altKey:
      case key === "Enter":
        if (cur >= 0) ops.open(b, rows[cur]);
        break;
      case key === " " && mod:
        if (b.cursor) b.toggle(b.cursor);
        break;
      case key === " ":
        if (cur === -1 && rows.length) moveTo(0, false);
        ops.setPreview(b, !b.previewOpen);
        break;
      case key === "F2":
        if (cur >= 0 && !b.search) ops.startRename(b, rows[cur]);
        break;
      case key === "Delete" || (isMac && key === "Backspace" && mod):
        if (sel.length) void ops.remove(b, sel);
        break;
      case mod && key.toLowerCase() === "a":
        b.selectAll();
        break;
      case mod && key.toLowerCase() === "c":
        if (sel.length) ops.copyToClipboard(b, sel);
        break;
      case mod && key.toLowerCase() === "v":
        void ops.paste(b);
        break;
      case key === "ContextMenu" || (key === "F10" && e.shiftKey): {
        const el = grid?.querySelector<HTMLElement>(`#${CSS.escape(rowId(cur))}`);
        const r = el?.getBoundingClientRect();
        ops.showMenu(b, sel, r ? { x: r.left + 40, y: r.bottom } : grid!);
        break;
      }
      case key === "Escape":
        if (b.search) b.cancelSearch();
        else if (b.selected.size) b.clearSelection();
        else return;
        break;
      case key.length === 1 && !mod && !e.altKey && /\S/.test(key):
        typeAhead(key);
        break;
      default:
        return;
    }
    e.preventDefault();
    e.stopPropagation();
  }

  // ------------------------------------------------------ inline inputs

  let createName = $state("");
  /** Guards against Enter + the blur that follows unmounting the input. */
  let committing = false;

  function selectStem(node: HTMLInputElement) {
    // Wait a frame so a value set by an effect is in place.
    requestAnimationFrame(() => {
      node.focus();
      const dot = node.value.lastIndexOf(".");
      node.setSelectionRange(0, dot > 0 ? dot : node.value.length);
    });
  }

  async function commitRename(entry: FileEntry, value: string, refocus: boolean) {
    if (committing || b.renaming !== entry.path) return;
    committing = true;
    try {
      if ((await ops.rename(b, entry, value)) && refocus) b.focusTick++;
    } finally {
      committing = false;
    }
  }

  function onRenameKey(e: KeyboardEvent, entry: FileEntry) {
    e.stopPropagation();
    if (e.key === "Enter") {
      e.preventDefault();
      void commitRename(entry, (e.currentTarget as HTMLInputElement).value, true);
    } else if (e.key === "Escape") {
      e.preventDefault();
      b.renaming = null;
      b.focusTick++;
    }
  }

  function onRenameBlur(e: FocusEvent, entry: FileEntry) {
    void commitRename(entry, (e.currentTarget as HTMLInputElement).value, false);
  }

  /** Name that just failed to create; blurring with it cancels instead of retrying. */
  let failedName: string | null = null;

  async function commitCreate(fromKey: boolean) {
    const kind = b.creating;
    if (committing || !kind) return;
    if (!createName.trim() || (!fromKey && createName === failedName)) {
      b.creating = null;
      failedName = null;
      return;
    }
    committing = true;
    try {
      if (await ops.create(b, kind, createName)) {
        createName = "";
        failedName = null;
        if (fromKey) b.focusTick++;
      } else failedName = createName;
    } finally {
      committing = false;
    }
  }

  function onCreateKey(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === "Enter") {
      e.preventDefault();
      void commitCreate(true);
    } else if (e.key === "Escape") {
      e.preventDefault();
      b.creating = null;
      createName = "";
      b.focusTick++;
    }
  }

  $effect.pre(() => {
    if (b.creating) createName = b.creating === "dir" ? "untitled folder" : "untitled.txt";
  });

  // ------------------------------------------------------------- drops

  const dropTarget = $derived(dnd.file?.target ?? dnd.os?.target ?? null);
  const dropRow = $derived(dropTarget?.paneId === b.tabId ? dropTarget.rowPath : null);
  const dragging = $derived(dnd.file?.sourcePane === b.tabId ? new Set(dnd.file.entries.map((e) => e.path)) : null);

  const ariaSort = (key: SortKey) =>
    filePrefs.sortKey === key ? (filePrefs.sortDir === "asc" ? "ascending" : "descending") : "none";

  const errorIcon = $derived(
    b.error?.kind === "permissionDenied" || /permission denied/i.test(b.error?.message ?? "")
      ? Lock
      : b.error?.kind === "notFound" || /no such file/i.test(b.error?.message ?? "")
        ? FolderX
        : CircleAlert,
  );
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions, a11y_click_events_have_key_events -->
<div
  bind:this={grid}
  bind:clientHeight={viewH}
  bind:clientWidth={width}
  class="grid"
  class:searching={!!b.search}
  role="grid"
  tabindex="0"
  aria-label={b.search ? `Search results for ${b.search.query}` : `Contents of ${b.path ?? "folder"}`}
  aria-multiselectable="true"
  aria-rowcount={rows.length + 1}
  aria-colcount={cols.length}
  aria-activedescendant={cursorIndex >= 0 && cursorIndex >= first && cursorIndex < last ? rowId(cursorIndex) : undefined}
  aria-busy={b.loading}
  data-autofocus-panel
  style:--cols={template}
  onscroll={(e) => (scrollTop = e.currentTarget.scrollTop)}
  {onkeydown}
  {onclick}
  {ondblclick}
  {onpointerdown}
  {oncontextmenu}
>
  <div class="head" role="rowgroup">
    <div class="row head-row" role="row" aria-rowindex={1}>
      {#each cols as c (c.key)}
        {#if c.key === "folder"}
          <div class="th" role="columnheader"><span class="th-label">Folder</span></div>
        {:else}
          {@const key = c.key}
          <div class="th" class:end={c.align === "end"} role="columnheader" aria-sort={ariaSort(key)}>
            <button class="th-btn" tabindex="-1" onclick={(e) => (e.stopPropagation(), sortBy(key))}>
              <span class="th-label">{c.label}</span>
              {#if filePrefs.sortKey === key}
                <span class="sort-ind">
                  {#if filePrefs.sortDir === "asc"}<ArrowUp size={11} strokeWidth={2.5} />{:else}<ArrowDown size={11} strokeWidth={2.5} />{/if}
                </span>
              {/if}
            </button>
          </div>
        {/if}
      {/each}
    </div>
  </div>

  {#if b.creating}
    <div class="row create-row" role="row">
      <div class="td name" role="gridcell">
        <span class="icon tone-{b.creating === 'dir' ? 'folder' : 'plain'}">
          {#if b.creating === "dir"}<Folder size={16} />{:else}<File size={16} />{/if}
        </span>
        <input
          class="inline-input"
          bind:value={createName}
          aria-label={b.creating === "dir" ? "New folder name" : "New file name"}
          spellcheck="false"
          autocomplete="off"
          use:selectStem
          onkeydown={onCreateKey}
          onblur={() => void commitCreate(false)}
        />
      </div>
    </div>
  {/if}

  {#if rows.length > 0}
    <div class="body" role="rowgroup" style:height="{rows.length * RH}px">
      {#each windowRows as { e, i } (e.path)}
        {@const ic = iconFor(e)}
        {@const selected = b.selected.has(e.path)}
        {@const dirLike = isDirLike(e)}
        {@const broken = e.kind === "symlink" && e.targetKind == null}
        <div
          class="row"
          class:selected
          class:cursor={i === cursorIndex}
          class:hidden-entry={e.hidden}
          class:drop={dropRow === e.path}
          class:drag-src={dragging?.has(e.path)}
          id={rowId(i)}
          role="row"
          aria-rowindex={i + 2}
          aria-selected={selected}
          data-index={i}
          data-dir-path={dirLike ? e.path : undefined}
          style:top="{i * RH}px"
        >
          <div class="td name" role="gridcell">
            <span class="icon tone-{ic.tone}"><ic.icon size={16} /></span>
            {#if b.renaming === e.path}
              <input
                class="inline-input"
                value={e.name}
                aria-label="Rename {e.name}"
                spellcheck="false"
                autocomplete="off"
                use:selectStem
                onkeydown={(ev) => onRenameKey(ev, e)}
                onblur={(ev) => onRenameBlur(ev, e)}
              />
            {:else}
              <span class="label" title={e.name}>{e.name}</span>
              {#if e.kind === "symlink"}
                <span class="link" class:broken title={broken ? `Broken link → ${e.linkTarget ?? "?"}` : `Link → ${e.linkTarget ?? "?"}`}>
                  <Link2 size={11} />
                  {#if broken}<span>broken</span>{/if}
                </span>
              {/if}
            {/if}
          </div>
          {#if has("folder")}
            {@const rel = relative(b.search?.root ?? "/", dirname(e.path) ?? "/")}
            <div class="td folder" role="gridcell" title={dirname(e.path) ?? ""}>{rel || "."}</div>
          {/if}
          {#if has("size")}
            <div class="td size num" role="gridcell">
              {#if dirLike}<span class="dim">—</span>{:else}{formatBytes(e.size)}{/if}
            </div>
          {/if}
          {#if has("modified")}
            <div class="td" role="gridcell" title={formatExact(e.modified)}>{formatModified(e.modified, now)}</div>
          {/if}
          {#if has("permissions")}
            <div class="td perms mono" role="gridcell" title={e.permissions != null ? octal(e.permissions) : undefined}>
              {permString(e.permissions, e.kind).slice(1)}
            </div>
          {/if}
          {#if has("owner")}
            <div class="td owner" role="gridcell" title={e.owner || e.group ? `${e.owner ?? "?"}:${e.group ?? "?"}` : undefined}>
              {#if e.owner || e.group}{e.owner ?? "—"}<span class="dim">{e.group ? ` · ${e.group}` : ""}</span>{:else}<span class="dim">—</span>{/if}
            </div>
          {/if}
        </div>
      {/each}
    </div>
  {:else if b.search}
    <div class="state" role="row">
      <div role="gridcell" class="state-inner">
        {#if b.search.running}
          <LoaderCircle size={20} class="spin" />
          <p class="state-title">Searching…</p>
        {:else if b.search.error}
          <span class="state-icon danger"><CircleAlert size={20} /></span>
          <p class="state-title">Search failed</p>
          <p class="state-text">{b.search.error}</p>
        {:else}
          <span class="state-icon"><SearchX size={20} /></span>
          <p class="state-title">No matches</p>
          <p class="state-text">Nothing named “{b.search.query}” under {b.search.root}.</p>
        {/if}
      </div>
    </div>
  {:else if b.error && !b.listing}
    {@const Icon = errorIcon}
    <div class="state" role="row">
      <div role="gridcell" class="state-inner">
        <span class="state-icon danger"><Icon size={20} /></span>
        <p class="state-title">
          {#if Icon === Lock}Permission denied{:else if Icon === FolderX}Folder not found{:else}Couldn’t open this folder{/if}
        </p>
        <p class="state-text selectable">{b.error.message}</p>
        <div class="state-actions">
          {#if b.canBack}<button class="btn btn-sm" onclick={() => b.back()}>Go back</button>{/if}
          {#if b.parent}<button class="btn btn-sm" onclick={() => b.up()}>Go up</button>{/if}
          <button class="btn btn-sm" onclick={() => b.refresh()}>Retry</button>
        </div>
      </div>
    </div>
  {:else if !b.listing}
    <div class="skeleton" aria-hidden="true">
      {#each Array(8) as _, i (i)}
        <div class="sk"><span class="sk-icon"></span><span class="sk-bar" style:width="{30 + ((i * 37) % 45)}%"></span></div>
      {/each}
    </div>
  {:else if !b.creating}
    <div class="state" role="row">
      <div role="gridcell" class="state-inner">
        {#if b.filter.trim()}
          <span class="state-icon"><SearchX size={20} /></span>
          <p class="state-title">No items match “{b.filter.trim()}”</p>
          <div class="state-actions"><button class="btn btn-sm" onclick={() => (b.filter = "")}>Clear filter</button></div>
        {:else if b.hiddenCount > 0}
          <span class="state-icon"><FolderOpen size={20} /></span>
          <p class="state-title">Only hidden items</p>
          <p class="state-text">This folder contains {b.hiddenCount} hidden {b.hiddenCount === 1 ? "item" : "items"}.</p>
          <div class="state-actions">
            <button class="btn btn-sm" onclick={() => ((filePrefs.showHidden = true), filePrefs.save())}>Show hidden files</button>
          </div>
        {:else}
          <span class="state-icon"><FolderOpen size={20} /></span>
          <p class="state-title">This folder is empty</p>
          <p class="state-text">Drop files here or create something new.</p>
          <div class="state-actions">
            <button class="btn btn-sm" onclick={() => ops.startCreate(b, "dir")}><FolderPlus size={13} /> New folder</button>
          </div>
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .grid {
    position: relative;
    flex: 1;
    min-height: 0;
    min-width: 0;
    overflow-y: auto;
    overflow-x: hidden;
    contain: strict;
    outline: none;
  }
  .row {
    display: grid;
    grid-template-columns: var(--cols);
    align-items: center;
    height: 28px;
    padding: 0 8px;
  }
  .head {
    position: sticky;
    top: 0;
    z-index: 2;
    background: var(--bg);
  }
  .head-row {
    height: 30px;
    border-bottom: 1px solid var(--border-subtle);
  }
  .th {
    display: flex;
    min-width: 0;
    padding: 0 6px;
  }
  .th.end {
    justify-content: flex-end;
  }
  .th-label {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .th > .th-label {
    color: var(--text-subtle);
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
  }
  .th-btn {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    min-width: 0;
    height: 22px;
    margin: 0 -6px;
    padding: 0 6px;
    border: 0;
    border-radius: var(--radius-xs);
    background: transparent;
    color: var(--text-subtle);
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
  }
  .th-btn:hover {
    background: var(--bg-hover);
    color: var(--text-muted);
  }
  .th[aria-sort="ascending"] .th-btn,
  .th[aria-sort="descending"] .th-btn {
    color: var(--text-muted);
  }
  .sort-ind {
    display: flex;
    color: var(--text-subtle);
  }

  .body {
    position: relative;
  }
  .body .row {
    position: absolute;
    left: 4px;
    right: 4px;
    padding: 0 4px;
    border-radius: var(--radius-sm);
    color: var(--text);
  }
  .body .row:hover {
    background: var(--bg-hover);
  }
  .body .row.selected {
    background: var(--bg-selected);
  }
  .grid:focus-visible .row.cursor,
  .grid:focus .row.cursor {
    box-shadow: inset 0 0 0 1px var(--focus-ring);
  }
  .grid:not(:focus-within) .row.selected {
    background: var(--bg-active);
  }
  .row.hidden-entry .label,
  .row.hidden-entry .icon {
    opacity: 0.55;
  }
  .row.drop {
    background: var(--accent-soft) !important;
    box-shadow: inset 0 0 0 1.5px var(--accent) !important;
  }
  .row.drag-src {
    opacity: 0.5;
  }
  .create-row {
    margin: 0 4px;
    padding: 0 4px;
    border-radius: var(--radius-sm);
    background: var(--bg-selected);
  }
  .td {
    min-width: 0;
    padding: 0 6px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .td.name {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--text);
    font-size: var(--text-base);
    padding-left: 4px;
  }
  .td.num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .td.perms {
    font-size: 11.5px;
    letter-spacing: 0.02em;
  }
  .td.folder {
    color: var(--text-subtle);
  }
  .dim {
    color: var(--text-subtle);
  }
  .label {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .icon {
    display: flex;
    flex: none;
  }
  .tone-folder {
    color: #3b82f6;
  }
  .tone-image {
    color: #a855f7;
  }
  .tone-code {
    color: #0d9488;
  }
  .tone-archive {
    color: #d97706;
  }
  .tone-media {
    color: #e11d48;
  }
  .tone-doc,
  .tone-plain {
    color: var(--text-muted);
  }
  .tone-key {
    color: #ca8a04;
  }
  .tone-exec {
    color: #16a34a;
  }
  .tone-broken {
    color: var(--danger);
  }
  :global([data-theme="dark"]) .tone-folder {
    color: #60a5fa;
  }
  :global([data-theme="dark"]) .tone-image {
    color: #c084fc;
  }
  :global([data-theme="dark"]) .tone-code {
    color: #2dd4bf;
  }
  :global([data-theme="dark"]) .tone-archive {
    color: #fbbf24;
  }
  :global([data-theme="dark"]) .tone-media {
    color: #fb7185;
  }
  :global([data-theme="dark"]) .tone-key {
    color: #facc15;
  }
  :global([data-theme="dark"]) .tone-exec {
    color: #4ade80;
  }
  .link {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    flex: none;
    color: var(--text-subtle);
    font-size: var(--text-xs);
  }
  .link.broken {
    padding: 0 6px 0 5px;
    height: 17px;
    border-radius: var(--radius-full);
    background: var(--danger-soft);
    color: var(--danger-text);
  }
  .inline-input:focus,
  .inline-input:focus-visible {
    outline: none;
  }
  .inline-input {
    flex: 1;
    min-width: 0;
    height: 22px;
    margin-left: -4px;
    padding: 0 4px;
    border: 1px solid var(--accent);
    border-radius: var(--radius-xs);
    background: var(--bg-input);
    box-shadow: 0 0 0 2px var(--accent-soft);
    color: var(--text);
    font-size: var(--text-base);
  }

  .state {
    display: grid;
    place-items: center;
    min-height: calc(100% - 30px);
    padding: 24px;
  }
  .state-inner {
    display: flex;
    flex-direction: column;
    align-items: center;
    max-width: 360px;
    text-align: center;
    color: var(--text-muted);
  }
  .state-icon {
    display: grid;
    place-items: center;
    width: 40px;
    height: 40px;
    margin-bottom: 10px;
    border-radius: 50%;
    background: var(--bg-subtle);
    color: var(--text-subtle);
  }
  .state-icon.danger {
    background: var(--danger-soft);
    color: var(--danger-text);
  }
  .state-title {
    margin-top: 4px;
    color: var(--text);
    font-weight: var(--weight-semibold);
  }
  .state-text {
    margin-top: 2px;
    font-size: var(--text-sm);
    overflow-wrap: anywhere;
  }
  .state-actions {
    display: flex;
    gap: 6px;
    margin-top: 14px;
  }

  .skeleton {
    padding: 6px 12px;
  }
  .sk {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 28px;
  }
  .sk-icon {
    width: 16px;
    height: 14px;
    border-radius: 3px;
    background: var(--bg-active);
  }
  .sk-bar {
    height: 9px;
    border-radius: 4px;
    background: var(--bg-active);
    opacity: 0.8;
  }
</style>
