<!--
  Bottom panel listing transfers (toggled from the sidebar footer,
  Ctrl/Cmd+Shift+J) and, on a second tab shown while there are any, files
  being edited in local apps.
-->
<script lang="ts">
  import ArrowDown from "@lucide/svelte/icons/arrow-down";
  import ArrowLeftRight from "@lucide/svelte/icons/arrow-left-right";
  import ArrowUp from "@lucide/svelte/icons/arrow-up";
  import Check from "@lucide/svelte/icons/check";
  import ChevronDown from "@lucide/svelte/icons/chevron-down";
  import Copy from "@lucide/svelte/icons/copy";
  import FolderOpen from "@lucide/svelte/icons/folder-open";
  import Pause from "@lucide/svelte/icons/pause";
  import Play from "@lucide/svelte/icons/play";
  import RotateCw from "@lucide/svelte/icons/rotate-cw";
  import X from "@lucide/svelte/icons/x";
  import EditList from "./EditList.svelte";
  import { machineName, openFiles } from "../files/ops";
  import { edits } from "../stores/edits.svelte";
  import { filePrefs } from "../stores/files.svelte";
  import { isActive, PANEL_MAX_FRACTION, PANEL_MIN, transfers } from "../stores/transfers.svelte";
  import type { TransferInfo } from "../types";
  import { sentence } from "../util/errors";
  import { formatBytes, formatDuration, formatSpeed, plural } from "../util/format";
  import { basename } from "../util/paths";
  import { modKey } from "../util/platform";

  function direction(t: TransferInfo) {
    const from = t.sourceHost;
    const to = t.destination.hostId;
    if (from === to) return { icon: Copy, label: "Copy" };
    if (from === null) return { icon: ArrowUp, label: "Upload" };
    if (to === null) return { icon: ArrowDown, label: "Download" };
    return { icon: ArrowLeftRight, label: "Remote to remote" };
  }

  function fraction(t: TransferInfo): number | null {
    if (t.state === "done") return 1;
    if (t.totalBytes == null || t.totalBytes <= 0) return null;
    return Math.min(1, t.doneBytes / t.totalBytes);
  }

  function meta(t: TransferInfo): string {
    switch (t.state) {
      case "queued":
        return "Queued";
      case "scanning":
        return t.totalFiles != null ? `Scanning · ${plural(t.totalFiles, "file")}` : "Scanning…";
      case "running": {
        if (!t.bytesPerSec) return "Starting…";
        const left = t.totalBytes != null ? (t.totalBytes - t.doneBytes) / t.bytesPerSec : NaN;
        return `${formatSpeed(t.bytesPerSec)}${Number.isFinite(left) ? ` · ${formatDuration(left)} left` : ""}`;
      }
      case "paused":
        return "Paused";
      case "waitingForUser":
        return "Waiting for you";
      case "done": {
        const secs = t.finishedAt ? (t.finishedAt - t.createdAt) / 1000 : null;
        return `Done${secs != null ? ` in ${formatDuration(secs)}` : ""}`;
      }
      case "failed":
        return t.resumable ? "Failed · can resume" : "Failed";
      case "cancelled":
        return "Cancelled";
    }
  }

  function amounts(t: TransferInfo): string {
    const bytes = t.totalBytes != null ? `${formatBytes(t.doneBytes)} of ${formatBytes(t.totalBytes)}` : formatBytes(t.doneBytes);
    const filesPart =
      t.totalFiles != null && t.totalFiles > 1 ? ` · ${t.doneFiles.toLocaleString()} of ${plural(t.totalFiles, "file")}` : "";
    const skipped = t.skippedFiles ? ` · ${t.skippedFiles.toLocaleString()} skipped` : "";
    return bytes + filesPart + skipped;
  }

  const summary = $derived.by(() => {
    const parts: string[] = [];
    if (transfers.active.length) parts.push(`${transfers.active.length} active`);
    if (transfers.failed.length) parts.push(`${transfers.failed.length} failed`);
    return parts.join(" · ");
  });

  // ------------------------------------------------------------- resize
  let resizing = $state(false);
  function onresizestart(e: PointerEvent) {
    e.preventDefault();
    resizing = true;
    const startY = e.clientY;
    const startH = filePrefs.transfersHeight;
    const target = e.currentTarget as HTMLElement;
    target.setPointerCapture(e.pointerId);
    const max = () => Math.max(PANEL_MIN, window.innerHeight * PANEL_MAX_FRACTION);
    const move = (ev: PointerEvent) => {
      filePrefs.transfersHeight = Math.round(Math.min(max(), Math.max(PANEL_MIN, startH - (ev.clientY - startY))));
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

  function onresizekey(e: KeyboardEvent) {
    const max = Math.max(PANEL_MIN, window.innerHeight * PANEL_MAX_FRACTION);
    if (e.key === "ArrowUp") filePrefs.transfersHeight = Math.min(max, filePrefs.transfersHeight + 24);
    else if (e.key === "ArrowDown") filePrefs.transfersHeight = Math.max(PANEL_MIN, filePrefs.transfersHeight - 24);
    else return;
    e.preventDefault();
    filePrefs.save();
  }

  const showTabs = $derived(edits.list.length > 0 || transfers.tab === "editing");
  const tab = $derived(showTabs ? transfers.tab : "transfers");

  const height = $derived(Math.min(filePrefs.transfersHeight, Math.max(PANEL_MIN, (typeof window !== "undefined" ? window.innerHeight : 800) * PANEL_MAX_FRACTION)));
</script>

{#if transfers.open}
  <section class="panel" class:resizing style:height="{height}px" aria-label={tab === "editing" ? "Editing" : "Transfers"}>
    <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
    <div
      class="resizer"
      role="separator"
      aria-orientation="horizontal"
      aria-label="Resize transfers panel"
      aria-valuenow={height}
      tabindex="0"
      onpointerdown={onresizestart}
      onkeydown={onresizekey}
    ></div>
    <header class="head">
      {#if showTabs}
        <div class="tabs" role="tablist" aria-label="Panel">
          <button class="tab" role="tab" aria-selected={tab === "transfers"} onclick={() => (transfers.tab = "transfers")}>
            Transfers{#if transfers.active.length}<span class="count">{transfers.active.length}</span>{/if}
          </button>
          <button class="tab" role="tab" aria-selected={tab === "editing"} onclick={() => (transfers.tab = "editing")}>
            Editing<span class="count" class:warn={edits.attention.length > 0}>{edits.list.length}</span>
          </button>
        </div>
      {:else}
        <h2>Transfers</h2>
      {/if}
      {#if tab === "transfers" && summary}<span class="summary">{summary}</span>{/if}
      <span class="spacer"></span>
      {#if tab === "transfers"}
        <button class="btn btn-ghost btn-sm" disabled={transfers.finishedCount === 0} onclick={() => transfers.clearFinished()}>
          Clear finished
        </button>
      {/if}
      <button class="icon-btn" aria-label="Hide transfers" title="Hide ({modKey}+Shift+J)" onclick={() => transfers.setOpen(false)}>
        <ChevronDown size={16} />
      </button>
    </header>
    {#if tab === "editing"}
      <EditList />
    {:else}
    <div class="list" role="list">
      {#each transfers.list as t (t.id)}
        {@const dir = direction(t)}
        {@const f = fraction(t)}
        <div class="item state-{t.state}" role="listitem">
          <span class="dir" title={dir.label}>
            {#if t.state === "done"}<Check size={15} strokeWidth={2.5} />{:else}<dir.icon size={15} />{/if}
          </span>
          <div class="main">
            <div class="line1">
              <span class="label" title={t.label}>{t.label}</span>
              <span class="route" title="{machineName(t.sourceHost)} → {machineName(t.destination.hostId)}: {t.destination.path}">
                {machineName(t.sourceHost)} <span class="arrow">→</span> {machineName(t.destination.hostId)}
                <span class="dest">· {t.background ? "for dragging out" : basename(t.destination.path)}</span>
              </span>
            </div>
            <div class="bar" class:indeterminate={f == null && isActive(t)} role="progressbar" aria-label="Progress of {t.label}" aria-valuemin={0} aria-valuemax={100} aria-valuenow={f == null ? undefined : Math.round(f * 100)}>
              <span style:width="{(f ?? 0) * 100}%"></span>
            </div>
            <div class="line3">
              {#if t.state === "failed"}
                <span class="error" title={t.message}>{sentence(t.message)}</span>
              {:else}
                <span class="amounts">{amounts(t)}</span>
                {#if t.currentFile && (t.state === "running" || t.state === "waitingForUser")}
                  <span class="current" title={t.currentFile}>{t.currentFile}</span>
                {/if}
              {/if}
            </div>
          </div>
          <div class="meta">
            {#if f != null && isActive(t)}<span class="pct">{Math.floor(f * 100)}%</span>{/if}
            <span class="status">{meta(t)}</span>
          </div>
          <div class="actions">
            {#if t.state === "running" || t.state === "scanning" || t.state === "queued"}
              <button class="icon-btn" aria-label="Pause {t.label}" title="Pause" disabled={t.state !== "running"} onclick={() => transfers.pause(t.id)}><Pause size={14} /></button>
            {:else if t.state === "paused"}
              <button class="icon-btn" aria-label="Resume {t.label}" title="Resume" onclick={() => transfers.resume(t.id)}><Play size={14} /></button>
            {:else if t.state === "failed" || t.state === "cancelled"}
              <button class="icon-btn" aria-label="{t.state === 'failed' && t.resumable ? 'Resume' : 'Retry'} {t.label}" title={t.state === "failed" && t.resumable ? "Resume" : "Retry"} onclick={() => transfers.retry(t.id)}>
                <RotateCw size={14} />
              </button>
            {/if}
            {#if isActive(t)}
              <button class="icon-btn" aria-label="Cancel {t.label}" title="Cancel" onclick={() => transfers.cancel(t.id)}><X size={14} /></button>
            {:else if !t.background}
              <button class="icon-btn" aria-label="Show {t.label} in folder" title="Open destination folder" onclick={() => openFiles(t.destination.hostId, { path: t.destination.path })}>
                <FolderOpen size={14} />
              </button>
            {/if}
          </div>
        </div>
      {:else}
        <div class="empty">
          <p class="empty-title">No transfers yet</p>
          <p class="empty-text">Drag files between panes, paste with {modKey}+V, or use Download to… to start one.</p>
        </div>
      {/each}
    </div>
    {/if}
  </section>
{/if}

<style>
  .panel {
    container: transfers / inline-size;
    position: relative;
    display: flex;
    flex-direction: column;
    flex: none;
    min-height: 0;
    border-top: 1px solid var(--border);
    background: var(--bg);
  }
  .resizer {
    position: absolute;
    left: 0;
    right: 0;
    top: -4px;
    height: 7px;
    cursor: row-resize;
    z-index: 7;
  }
  .resizer:hover,
  .resizer:focus-visible,
  .resizing .resizer {
    outline: none;
    background: linear-gradient(to bottom, transparent 3px, var(--accent) 3px, var(--accent) 5px, transparent 5px);
    opacity: 0.7;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 36px;
    padding: 0 8px 0 14px;
    border-bottom: 1px solid var(--border-subtle);
    background: var(--bg-sidebar);
    flex: none;
  }
  h2 {
    font-size: var(--text-sm);
    font-weight: var(--weight-semibold);
  }
  .tabs {
    display: flex;
    align-items: center;
    gap: 2px;
    margin-left: -6px;
  }
  .tab {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 26px;
    padding: 0 8px;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text-muted);
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
  }
  .tab:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  .tab[aria-selected="true"] {
    background: var(--bg-active);
    color: var(--text);
    font-weight: var(--weight-semibold);
  }
  .count {
    display: inline-grid;
    place-items: center;
    min-width: 16px;
    height: 16px;
    padding: 0 4px;
    border-radius: var(--radius-full);
    background: var(--accent-soft);
    color: var(--accent-text);
    font-size: 10.5px;
    font-weight: var(--weight-semibold);
    font-variant-numeric: tabular-nums;
  }
  .count.warn {
    background: var(--warning-soft);
    color: var(--warning-text);
  }
  .summary {
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .spacer {
    flex: 1;
  }
  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 4px 0;
  }
  .item {
    display: grid;
    grid-template-columns: 30px minmax(0, 1fr) auto auto;
    align-items: center;
    gap: 12px;
    padding: 8px 10px 8px 14px;
  }
  .item + .item {
    border-top: 1px solid var(--border-subtle);
  }
  .dir {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: var(--radius-md);
    background: var(--accent-soft);
    color: var(--accent-text);
  }
  .state-done .dir {
    background: var(--success-soft);
    color: var(--success-text);
  }
  .state-failed .dir {
    background: var(--danger-soft);
    color: var(--danger-text);
  }
  .state-paused .dir,
  .state-cancelled .dir,
  .state-queued .dir {
    background: var(--bg-active);
    color: var(--text-muted);
  }
  .state-waitingForUser .dir {
    background: var(--warning-soft);
    color: var(--warning-text);
  }
  .main {
    min-width: 0;
  }
  .line1 {
    display: flex;
    align-items: baseline;
    gap: 8px;
    min-width: 0;
  }
  .label {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-weight: var(--weight-medium);
  }
  .route {
    flex: 0 10 auto;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .arrow,
  .dest {
    color: var(--text-subtle);
  }
  .bar {
    position: relative;
    height: 4px;
    margin: 6px 0 5px;
    border-radius: 2px;
    background: var(--bg-active);
    overflow: hidden;
  }
  .bar span {
    position: absolute;
    left: 0;
    top: 0;
    bottom: 0;
    border-radius: 2px;
    background: var(--accent);
    transition: width 220ms linear;
  }
  .state-done .bar span {
    background: var(--success);
  }
  .state-failed .bar span {
    background: var(--danger);
  }
  .state-paused .bar span,
  .state-cancelled .bar span {
    background: var(--text-subtle);
  }
  .state-waitingForUser .bar span {
    background: var(--warning);
  }
  .bar.indeterminate span {
    width: 30% !important;
    animation: indet 1.1s var(--ease-out) infinite;
  }
  @keyframes indet {
    from {
      left: -30%;
    }
    to {
      left: 100%;
    }
  }
  .line3 {
    display: flex;
    gap: 8px;
    min-width: 0;
    color: var(--text-subtle);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
  .amounts {
    flex: none;
  }
  .current {
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    direction: rtl;
    text-align: left;
  }
  .error {
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--danger-text);
  }
  .meta {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 1px;
    min-width: 72px;
    font-variant-numeric: tabular-nums;
  }
  .pct {
    font-weight: var(--weight-semibold);
    font-size: var(--text-sm);
  }
  .status {
    color: var(--text-muted);
    font-size: var(--text-xs);
    white-space: nowrap;
  }
  .state-failed .status {
    color: var(--danger-text);
  }
  .state-waitingForUser .status {
    color: var(--warning-text);
    font-weight: var(--weight-medium);
  }
  .actions {
    display: flex;
    gap: 2px;
  }
  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 2px;
    height: 100%;
    padding: 16px;
    text-align: center;
  }
  .empty-title {
    font-weight: var(--weight-medium);
  }
  .empty-text {
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  @container transfers (max-width: 620px) {
    .line1 {
      flex-direction: column;
      align-items: stretch;
      gap: 0;
    }
    .route {
      font-size: var(--text-xs);
    }
    .item {
      gap: 10px;
    }
    .meta {
      min-width: 0;
    }
  }
</style>
