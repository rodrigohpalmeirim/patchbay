<!-- The transfers panel's Editing tab: remote files open in local apps. -->
<script lang="ts">
  import Check from "@lucide/svelte/icons/check";
  import FileDiff from "@lucide/svelte/icons/file-diff";
  import FilePen from "@lucide/svelte/icons/file-pen";
  import FolderOpen from "@lucide/svelte/icons/folder-open";
  import LoaderCircle from "@lucide/svelte/icons/loader-circle";
  import RefreshCwOff from "@lucide/svelte/icons/refresh-cw-off";
  import RotateCw from "@lucide/svelte/icons/rotate-cw";
  import SquareArrowOutUpRight from "@lucide/svelte/icons/square-arrow-out-up-right";
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";
  import X from "@lucide/svelte/icons/x";
  import { machineName } from "../files/ops";
  import { editName, edits, failure } from "../stores/edits.svelte";
  import type { EditSession } from "../types";
  import { formatModified } from "../util/format";
  import { dirname } from "../util/paths";

  let now = $state(Date.now());
  $effect(() => {
    const t = setInterval(() => (now = Date.now()), 30_000);
    return () => clearInterval(t);
  });

  const ago = (ms: number) => {
    const s = formatModified(Math.floor(ms / 1000), now);
    return s.charAt(0).toLowerCase() + s.slice(1);
  };

  function detail(s: EditSession): string {
    switch (s.state) {
      case "synced":
        return s.lastUpload != null ? `Saved ${ago(s.lastUpload)}` : `Opened ${ago(s.at)} · saves upload automatically`;
      case "syncing":
        return "Downloading the server’s version…";
      case "uploading":
        return "Uploading your changes…";
      case "failed":
        return failure(s);
      case "conflict":
        return "Changed on the server too — choose which version to keep";
      case "stopped":
        return "Not syncing · your changes are only on this computer";
      case "closed":
        return "";
    }
  }

  const STATUS: Record<EditSession["state"], string> = {
    synced: "Synced",
    syncing: "Downloading",
    uploading: "Uploading",
    failed: "Not saved",
    conflict: "Conflict",
    stopped: "Stopped",
    closed: "",
  };
</script>

<div class="list" role="list">
  {#each edits.list as s (s.id)}
    {@const name = editName(s)}
    <div class="item state-{s.state}" role="listitem">
      <span class="icon" aria-hidden="true">
        {#if s.state === "conflict"}<FileDiff size={15} />{:else if s.state === "failed"}<TriangleAlert size={15} />{:else if s.state === "stopped"}<RefreshCwOff size={15} />{:else}<FilePen size={15} />{/if}
      </span>
      <div class="main">
        <div class="line1">
          <span class="label" title={s.remotePath}>{name}</span>
          <span class="route" title="{machineName(s.hostId)}: {s.remotePath}">
            {machineName(s.hostId)} <span class="dir">· {dirname(s.remotePath) ?? "/"}</span>
          </span>
        </div>
        <div class="line2" class:error={s.state === "failed"} title={s.state === "failed" ? s.message : undefined}>
          {detail(s)}
        </div>
      </div>
      <div class="meta">
        <span class="status">
          {#if s.state === "synced"}<Check size={12} strokeWidth={2.5} />{:else if s.state === "uploading" || s.state === "syncing"}<LoaderCircle size={12} class="spin" />{/if}
          {STATUS[s.state]}
        </span>
      </div>
      <div class="actions">
        {#if s.state === "conflict"}
          <button class="btn btn-sm" onclick={() => edits.retry(s)}>Resolve…</button>
        {:else if s.state === "failed"}
          <button class="btn btn-sm" onclick={() => edits.retry(s)}><RotateCw size={12} /> {s.recovered ? "Upload" : "Retry"}</button>
        {:else if s.state === "stopped"}
          <button class="btn btn-sm" onclick={() => edits.retry(s)} title="Upload your changes and keep syncing">Resume</button>
        {/if}
        {#if s.hostId}
          <button class="icon-btn" aria-label="Open {name} in its app" title="Open in its app" onclick={() => edits.reopen(s)}>
            <SquareArrowOutUpRight size={14} />
          </button>
        {/if}
        <button class="icon-btn" aria-label="Show the local copy of {name}" title="Show local copy" onclick={() => edits.reveal(s)}>
          <FolderOpen size={14} />
        </button>
        <button class="icon-btn" aria-label="Stop editing {name}" title="Stop editing" onclick={() => edits.stop(s)}>
          <X size={14} />
        </button>
      </div>
    </div>
  {:else}
    <div class="empty">
      <p class="empty-title">No files being edited</p>
      <p class="empty-text">Right-click a file on a server and choose Edit in default app. Every save is uploaded.</p>
    </div>
  {/each}
</div>

<style>
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
  .icon {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: var(--radius-md);
    background: var(--accent-soft);
    color: var(--accent-text);
  }
  .state-failed .icon {
    background: var(--danger-soft);
    color: var(--danger-text);
  }
  .state-conflict .icon {
    background: var(--warning-soft);
    color: var(--warning-text);
  }
  .state-stopped .icon {
    background: var(--bg-active);
    color: var(--text-muted);
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
  .dir {
    color: var(--text-subtle);
  }
  .line2 {
    margin-top: 2px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--text-subtle);
    font-size: var(--text-xs);
  }
  .line2.error {
    color: var(--danger-text);
  }
  .state-conflict .line2 {
    color: var(--warning-text);
  }
  .meta {
    min-width: 72px;
    text-align: right;
  }
  .status {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: var(--text-muted);
    font-size: var(--text-xs);
    white-space: nowrap;
  }
  .state-synced .status {
    color: var(--success-text);
  }
  .state-failed .status {
    color: var(--danger-text);
  }
  .state-conflict .status {
    color: var(--warning-text);
    font-weight: var(--weight-medium);
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .actions .btn {
    margin-right: 4px;
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
    .meta {
      display: none;
    }
  }
</style>
