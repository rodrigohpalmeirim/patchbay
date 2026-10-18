<!--
  Files tab: gates remote panes on the connection (inline connect state),
  then hands over to FilePane. Keeps the last listing on disconnect and
  shows a reconnect banner instead of dropping the tab.
-->
<script lang="ts">
  import CircleAlert from "@lucide/svelte/icons/circle-alert";
  import FolderOpen from "@lucide/svelte/icons/folder-open";
  import LoaderCircle from "@lucide/svelte/icons/loader-circle";
  import Plug from "@lucide/svelte/icons/plug";
  import RotateCw from "@lucide/svelte/icons/rotate-cw";
  import X from "@lucide/svelte/icons/x";
  import { onDestroy, untrack } from "svelte";
  import { editHost } from "../actions";
  import OsIcon from "../components/OsIcon.svelte";
  import FilePane from "../files/FilePane.svelte";
  import { connections } from "../stores/connections.svelte";
  import { files } from "../stores/files.svelte";
  import { prompts } from "../stores/prompts.svelte";
  import type { FilesTab } from "../stores/tabs.svelte";
  import type { Host } from "../types";
  import { errorTitle, sentence } from "../util/errors";
  import { address, isEditable } from "../util/format";

  let { tab, host, active }: { tab: FilesTab; host: Host | null; active: boolean } = $props();

  // A tab's machine never changes, so the browser is created once.
  const browser = untrack(() => files.ensure(tab));
  onDestroy(() => files.dispose(browser.tabId));

  const conn = $derived(host ? connections.get(host.id) : null);
  const phase = $derived(conn?.phase ?? "connected");
  const connected = $derived(phase === "connected");
  const waiting = $derived(host ? prompts.isWaiting(host.id) : false);
  const noSftp = $derived(!!conn?.capabilities && !conn.capabilities.sftp);

  $effect(() => {
    if (!connected) return;
    untrack(() => {
      if (!browser.listing || browser.stale) void browser.init();
    });
  });

  const stageLabel = $derived.by(() => {
    if (!conn) return "";
    if (waiting) return "Waiting for your input…";
    switch (conn.phase) {
      case "connecting":
        return conn.hop ? `Connecting via ${conn.hop}…` : `Connecting to ${host?.hostname}…`;
      case "authenticating":
        return "Authenticating…";
      case "reconnecting":
        return `Reconnecting (attempt ${conn.attempt})…`;
      case "disconnecting":
        return "Disconnecting…";
      default:
        return "";
    }
  });

  function connect() {
    if (host) void connections.connect(host.id);
  }
</script>

<div class="files-view">
  {#if host && noSftp}
    <div class="gate">
      <div class="gate-card">
        <span class="gate-icon warn"><CircleAlert size={20} /></span>
        <h2>SFTP isn’t available on {host.name}</h2>
        <p>The server doesn’t offer the SFTP subsystem, so its files can’t be browsed.</p>
      </div>
    </div>
  {:else if host && !connected && !browser.listing}
    <div class="gate" aria-live="polite">
      <div class="gate-card">
        <span class="gate-icon">
          {#if conn?.capabilities}<OsIcon os={conn.capabilities.os} size={20} />{:else}<FolderOpen size={20} />{/if}
        </span>
        <h2>Files on {host.name}</h2>
        <p class="addr mono">{address(host)}</p>
        {#if phase === "error" && conn?.error}
          <div class="callout" role="alert">
            <CircleAlert size={15} />
            <div>
              <div class="callout-title">{errorTitle(conn.error.kind)}</div>
              <div class="callout-text selectable">{sentence(conn.error.message)}</div>
            </div>
          </div>
          <div class="gate-actions">
            <button class="btn" onclick={() => editHost(host)}>{isEditable(host) ? "Edit host…" : "View settings…"}</button>
            <button class="btn btn-primary" onclick={connect}><RotateCw size={14} /> Try again</button>
          </div>
        {:else if stageLabel}
          <div class="progress" class:waiting>
            <LoaderCircle size={15} class="spin" />
            <span>{stageLabel}</span>
          </div>
          <div class="gate-actions">
            <button class="btn" onclick={() => void connections.disconnect(host.id)}><X size={14} /> Cancel</button>
          </div>
        {:else}
          <p class="muted">
            {#if conn?.lostReason}Connection lost: {sentence(conn.lostReason)}{:else}Connect to browse, upload and download files.{/if}
          </p>
          <div class="gate-actions">
            <button class="btn btn-primary btn-lg" onclick={connect} data-autofocus-panel><Plug size={15} /> Connect</button>
          </div>
        {/if}
      </div>
    </div>
  {:else}
    <FilePane {browser} {host} {active} {connected} {stageLabel} onreconnect={connect} />
  {/if}
</div>

<style>
  .files-view {
    height: 100%;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .gate {
    flex: 1;
    display: grid;
    place-items: center;
    padding: 24px;
    overflow-y: auto;
  }
  .gate-card {
    display: flex;
    flex-direction: column;
    align-items: center;
    max-width: 380px;
    text-align: center;
  }
  .gate-icon {
    display: grid;
    place-items: center;
    width: 48px;
    height: 48px;
    margin-bottom: 14px;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-subtle);
    color: var(--text-muted);
  }
  .gate-icon.warn {
    border-color: transparent;
    background: var(--warning-soft);
    color: var(--warning-text);
  }
  h2 {
    font-size: var(--text-lg);
    font-weight: var(--weight-semibold);
    letter-spacing: -0.01em;
  }
  p {
    margin-top: 4px;
    color: var(--text-muted);
  }
  .addr {
    font-size: 12px;
  }
  .muted {
    margin-top: 12px;
  }
  .gate-actions {
    display: flex;
    gap: 8px;
    margin-top: 18px;
  }
  .progress {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    margin-top: 16px;
    padding: 6px 12px;
    border-radius: var(--radius-full);
    background: var(--bg-subtle);
    color: var(--text-muted);
  }
  .progress.waiting {
    background: var(--warning-soft);
    color: var(--warning-text);
  }
  .callout {
    display: flex;
    gap: 10px;
    margin-top: 16px;
    padding: 12px;
    border-radius: var(--radius-md);
    background: var(--danger-soft);
    color: var(--danger-text);
    text-align: left;
  }
  .callout > :global(svg) {
    flex: none;
    margin-top: 2px;
  }
  .callout-title {
    font-weight: var(--weight-semibold);
  }
  .callout-text {
    margin-top: 2px;
    color: var(--text);
    overflow-wrap: anywhere;
  }
</style>
