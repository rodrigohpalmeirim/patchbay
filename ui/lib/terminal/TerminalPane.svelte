<!--
  One terminal pane: hosts the session's xterm element and its overlays
  (connect gate, failure, exit bar, find bar, link hint, bell flash).
  Absolutely positioned by TerminalView from the flattened pane layout.
-->
<script lang="ts">
  import CircleAlert from "@lucide/svelte/icons/circle-alert";
  import LoaderCircle from "@lucide/svelte/icons/loader-circle";
  import Plug from "@lucide/svelte/icons/plug";
  import RotateCw from "@lucide/svelte/icons/rotate-cw";
  import SquareTerminal from "@lucide/svelte/icons/square-terminal";
  import Unplug from "@lucide/svelte/icons/unplug";
  import X from "@lucide/svelte/icons/x";
  import { untrack } from "svelte";
  import { editHost } from "../actions";
  import { connections } from "../stores/connections.svelte";
  import { prompts } from "../stores/prompts.svelte";
  import { settings } from "../stores/settings.svelte";
  import type { TerminalTab } from "../stores/tabs.svelte";
  import type { TermSession } from "../stores/terminals.svelte";
  import type { Host } from "../types";
  import { errorTitle, sentence } from "../util/errors";
  import { isEditable } from "../util/format";
  import { isMac } from "../util/platform";
  import FindBar from "./FindBar.svelte";
  import { keyLabel } from "./keys";
  import type { Rect } from "./layout";
  import { closePane, focusPane, showPaneMenu } from "./ops";

  let {
    tab,
    session,
    host,
    rect,
    focused,
    multi,
    hidden,
  }: {
    tab: TerminalTab;
    session: TermSession;
    host: Host | null;
    rect: Rect;
    /** This pane has the tab's keyboard focus. */
    focused: boolean;
    /** The tab has more than one (visible) pane. */
    multi: boolean;
    /** Covered by a zoomed pane. */
    hidden: boolean;
  } = $props();

  let mount = $state<HTMLDivElement>();

  $effect(() => {
    if (mount) session.attach(mount);
  });

  // Settings and theme apply live.
  $effect(() => {
    void settings.resolvedTheme;
    void $state.snapshot(settings.terminal);
    untrack(() => session.applyPrefs());
  });

  // Connect-first: open the shell once the host is connected.
  const conn = $derived(host ? connections.get(host.id) : null);
  const phase = $derived(conn?.phase ?? "connected");
  const waitingInput = $derived(host ? prompts.isWaiting(host.id) : false);
  $effect(() => {
    if (session.status === "waiting" && phase === "connected") untrack(() => void session.open());
  });

  const stageLabel = $derived.by(() => {
    if (!conn || !host) return "";
    if (waitingInput) return "Waiting for your input…";
    switch (conn.phase) {
      case "connecting":
        return conn.hop ? `Connecting via ${conn.hop}…` : `Connecting to ${host.hostname}…`;
      case "authenticating":
        return "Authenticating…";
      case "reconnecting":
        return `Reconnecting (attempt ${conn.attempt})…`;
      case "disconnecting":
        return "Disconnecting…";
      case "connected":
        return "Starting shell…";
      default:
        return "";
    }
  });

  const exitTitle = $derived.by(() => {
    const ex = session.exit;
    if (!ex) return "";
    if (ex.code === null) {
      if (host && phase !== "connected") return ex.reason === "disconnected" ? `Disconnected from ${host.name}` : `Connection to ${host.name} lost`;
      return "Session ended";
    }
    return ex.code === 0 ? "Process exited" : `Process exited with code ${ex.code}`;
  });
  const exitDetail = $derived.by(() => {
    const ex = session.exit;
    if (!ex?.reason || ex.reason === "disconnected") return "";
    return sentence(ex.reason);
  });
  const exitBad = $derived(!!session.exit && session.exit.code !== 0);

  // Visual bell: brief flash.
  let flashing = $state(false);
  let flashTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    if (session.bellTick === 0) return;
    untrack(() => {
      flashing = true;
      clearTimeout(flashTimer);
      flashTimer = setTimeout(() => (flashing = false), 180);
    });
  });

  function oncontextmenu(e: MouseEvent) {
    e.preventDefault();
    focusPane(tab, session.paneId);
    showPaneMenu(tab, session.paneId, { x: e.clientX, y: e.clientY });
  }

  function onpointerdown() {
    if (tab.focusedPane !== session.paneId) focusPane(tab, session.paneId, false);
  }

  function connect() {
    if (host) void connections.connect(host.id);
  }

  function retry() {
    session.restart();
  }

  // Before the first shell runs the pane is empty, so a centered gate is
  // shown; afterwards the old output stays visible and a bottom bar is used.
  const showGate = $derived(
    !session.started && (session.status === "waiting" || session.status === "failed" || (session.status === "opening" && !!host)),
  );
  const restarting = $derived(session.started && (session.status === "waiting" || session.status === "opening" || session.status === "failed"));
  const linkHint = isMac ? "⌘+click to open" : "Ctrl+click to open";
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="pane"
  class:focused={focused && multi}
  class:dim={multi && !focused}
  class:covered={hidden}
  class:flash={flashing}
  style:left="{rect.x * 100}%"
  style:top="{rect.y * 100}%"
  style:width="{rect.w * 100}%"
  style:height="{rect.h * 100}%"
  data-terminal-pane={session.paneId}
  aria-hidden={hidden || undefined}
  {oncontextmenu}
  onpointerdowncapture={onpointerdown}
>
  <div class="mount" bind:this={mount}></div>

  {#if session.findOpen}
    <FindBar {session} />
  {/if}

  {#if session.linkHover}
    <div class="link-hint" style:left="{session.linkHover.x + 12}px" style:top="{session.linkHover.y + 16}px">
      <span class="mono uri">{session.linkHover.uri}</span>
      <span class="sep">·</span>
      {linkHint}
    </div>
  {/if}

  {#if showGate}
    <div class="gate" aria-live="polite">
      <div class="gate-card">
        {#if session.status === "failed" && session.error}
          <span class="gate-icon warn"><CircleAlert size={18} /></span>
          <div class="gate-title">{session.error.kind === "unsupported" ? "Shell not available" : "Couldn't start the shell"}</div>
          <p class="gate-text selectable">{sentence(session.error.message)}</p>
          {#if session.error.kind !== "unsupported"}
            <div class="gate-actions">
              <button class="btn btn-sm" onclick={() => closePane(tab, session.paneId)}>Close</button>
              <button class="btn btn-sm btn-primary" onclick={retry}><RotateCw size={13} /> Try again</button>
            </div>
          {:else}
            <div class="gate-actions">
              <button class="btn btn-sm" onclick={() => closePane(tab, session.paneId)}>Close</button>
            </div>
          {/if}
        {:else if host && phase === "error" && conn?.error}
          <span class="gate-icon warn"><CircleAlert size={18} /></span>
          <div class="gate-title">{errorTitle(conn.error.kind)}</div>
          <p class="gate-text selectable">{sentence(conn.error.message)}</p>
          <div class="gate-actions">
            <button class="btn btn-sm" onclick={() => editHost(host)}>{isEditable(host) ? "Edit host…" : "View settings…"}</button>
            <button class="btn btn-sm btn-primary" onclick={connect}><RotateCw size={13} /> Try again</button>
          </div>
        {:else if host && phase === "idle"}
          <span class="gate-icon"><SquareTerminal size={18} /></span>
          <div class="gate-title">Shell on {host.name}</div>
          <p class="gate-text">
            {#if conn?.lostReason}Connection lost: {sentence(conn.lostReason)}{:else}Connect to start a shell.{/if}
          </p>
          <div class="gate-actions">
            <button class="btn btn-sm btn-primary" onclick={connect}><Plug size={13} /> Connect</button>
          </div>
        {:else}
          {#if host}
            <span class="gate-icon"><SquareTerminal size={18} /></span>
            <div class="gate-title">Shell on {host.name}</div>
          {/if}
          <div class="progress" class:waiting={waitingInput}>
            <LoaderCircle size={14} class="spin" />
            <span>{stageLabel || "Starting shell…"}</span>
          </div>
          {#if host && phase !== "connected"}
            <div class="gate-actions">
              <button class="btn btn-sm" onclick={() => void connections.disconnect(host.id)}><X size={13} /> Cancel</button>
            </div>
          {/if}
        {/if}
      </div>
    </div>
  {/if}

  {#if session.status === "exited" && session.exit}
    <div class="exit-bar" class:bad={exitBad} role="status">
      <span class="exit-icon">
        {#if session.exit.code === null}<Unplug size={14} />{:else}<SquareTerminal size={14} />{/if}
      </span>
      <span class="exit-text">
        <strong>{exitTitle}</strong>
        {#if exitDetail}<span class="exit-detail">{exitDetail}</span>{/if}
      </span>
      <button class="btn btn-sm btn-ghost" onclick={() => closePane(tab, session.paneId)} title="Close pane ({keyLabel('closePane')})">Close</button>
      <button class="btn btn-sm btn-primary" onclick={retry}>
        <RotateCw size={13} />
        {host && phase !== "connected" ? "Reconnect" : "Restart"}
        <kbd class="kbd-on-accent">↵</kbd>
      </button>
    </div>
  {:else if restarting}
    <div class="exit-bar" class:bad={session.status === "failed" || phase === "error"} role="status" aria-live="polite">
      {#if session.status === "failed" && session.error}
        <span class="exit-icon"><CircleAlert size={14} /></span>
        <span class="exit-text">
          <strong>Couldn’t start the shell</strong>
          <span class="exit-detail">{sentence(session.error.message)}</span>
        </span>
        <button class="btn btn-sm btn-ghost" onclick={() => closePane(tab, session.paneId)}>Close</button>
        <button class="btn btn-sm btn-primary" onclick={retry}><RotateCw size={13} /> Try again</button>
      {:else if host && phase === "error" && conn?.error}
        <span class="exit-icon"><CircleAlert size={14} /></span>
        <span class="exit-text">
          <strong>{errorTitle(conn.error.kind)}</strong>
          <span class="exit-detail">{sentence(conn.error.message)}</span>
        </span>
        <button class="btn btn-sm btn-ghost" onclick={() => editHost(host)}>{isEditable(host) ? "Edit host…" : "View settings…"}</button>
        <button class="btn btn-sm btn-primary" onclick={connect}><RotateCw size={13} /> Try again</button>
      {:else if host && phase === "idle"}
        <span class="exit-icon"><Unplug size={14} /></span>
        <span class="exit-text"><strong>Not connected to {host.name}</strong></span>
        <button class="btn btn-sm btn-ghost" onclick={() => closePane(tab, session.paneId)}>Close</button>
        <button class="btn btn-sm btn-primary" onclick={connect}><Plug size={13} /> Reconnect <kbd class="kbd-on-accent">↵</kbd></button>
      {:else}
        <span class="exit-icon busy"><LoaderCircle size={14} class="spin" /></span>
        <span class="exit-text" class:waiting={waitingInput}><strong>{stageLabel || "Starting shell…"}</strong></span>
        {#if host && phase !== "connected"}
          <button class="btn btn-sm btn-ghost" onclick={() => void connections.disconnect(host.id)}><X size={13} /> Cancel</button>
        {/if}
      {/if}
    </div>
  {/if}
</div>

<style>
  .pane {
    position: absolute;
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
    background: var(--bg);
    container-type: inline-size;
  }
  .pane.covered {
    visibility: hidden;
    pointer-events: none;
  }
  .mount {
    position: relative;
    flex: 1;
    min-height: 0;
  }
  /* The session element xterm renders into (created outside Svelte). */
  .mount :global(.term-host) {
    position: absolute;
    inset: 0;
  }
  .mount :global(.term-host .xterm) {
    height: 100%;
    padding: 6px 0 2px 12px;
  }
  .mount :global(.xterm .xterm-viewport) {
    background-color: transparent !important;
  }
  /* Slimmer overlay scrollbar (xterm sizes it inline). */
  .mount :global(.xterm .xterm-scrollable-element > .scrollbar.vertical > .slider) {
    left: 5px !important;
    width: 6px !important;
    border-radius: 3px;
  }
  .mount :global(.xterm:focus),
  .mount :global(.xterm-helper-textarea:focus) {
    outline: none;
  }
  /* Inactive panes recede a little so the focused one is obvious. */
  .pane.dim .mount {
    opacity: 0.8;
    transition: opacity var(--dur-fast);
  }
  .pane.focused::after {
    content: "";
    position: absolute;
    inset: 0;
    border: 1px solid var(--accent);
    opacity: 0.55;
    pointer-events: none;
    z-index: 3;
  }
  .pane.flash::before {
    content: "";
    position: absolute;
    inset: 0;
    background: var(--accent-soft);
    pointer-events: none;
    z-index: 4;
  }

  .link-hint {
    position: absolute;
    z-index: 6;
    display: flex;
    align-items: center;
    gap: 6px;
    max-width: min(420px, calc(100% - 24px));
    padding: 3px 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-elevated);
    box-shadow: var(--shadow-md);
    color: var(--text-muted);
    font-size: var(--text-xs);
    white-space: nowrap;
    pointer-events: none;
  }
  .link-hint .uri {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--text);
  }
  .link-hint .sep {
    color: var(--text-subtle);
  }

  .gate {
    position: absolute;
    inset: 0;
    z-index: 5;
    display: grid;
    place-items: center;
    padding: 16px;
    background: var(--bg);
    overflow: auto;
  }
  .gate-card {
    display: flex;
    flex-direction: column;
    align-items: center;
    max-width: 340px;
    text-align: center;
  }
  .gate-icon {
    display: grid;
    place-items: center;
    width: 40px;
    height: 40px;
    margin-bottom: 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--bg-subtle);
    color: var(--text-muted);
  }
  .gate-icon.warn {
    border-color: transparent;
    background: var(--danger-soft);
    color: var(--danger-text);
  }
  .gate-title {
    font-weight: var(--weight-semibold);
  }
  .gate-text {
    margin-top: 4px;
    color: var(--text-muted);
    overflow-wrap: anywhere;
  }
  .gate-actions {
    display: flex;
    gap: 8px;
    margin-top: 14px;
  }
  .gate-title + .progress {
    margin-top: 12px;
  }
  .progress {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 6px 12px;
    border-radius: var(--radius-full);
    background: var(--bg-subtle);
    color: var(--text-muted);
  }
  .progress.waiting {
    background: var(--warning-soft);
    color: var(--warning-text);
  }

  .exit-bar {
    position: relative;
    z-index: 5;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: flex-end;
    gap: 6px 8px;
    flex: none;
    min-height: 40px;
    padding: 6px 8px 6px 12px;
    border-top: 1px solid var(--border);
    background: var(--bg-subtle);
    animation: rise var(--dur) var(--ease-out);
  }
  .exit-icon {
    display: grid;
    place-items: center;
    flex: none;
    width: 24px;
    height: 24px;
    border-radius: var(--radius-sm);
    background: var(--bg-active);
    color: var(--text-muted);
  }
  .exit-icon.busy {
    background: transparent;
    color: var(--accent-text);
  }
  .exit-text.waiting strong {
    color: var(--warning-text);
  }
  .exit-bar.bad .exit-icon {
    background: var(--warning-soft);
    color: var(--warning-text);
  }
  .exit-text {
    display: flex;
    align-items: baseline;
    gap: 8px;
    flex: 1 1 150px;
    min-width: 0;
    white-space: nowrap;
  }
  .exit-text strong {
    font-weight: var(--weight-semibold);
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .exit-detail {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .kbd-on-accent {
    height: 16px;
    min-width: 16px;
    margin-left: 2px;
    padding: 0 4px;
    border: 0;
    background: rgba(255, 255, 255, 0.22);
    color: inherit;
  }
  @container (max-width: 380px) {
    .exit-detail {
      display: none;
    }
    .exit-icon {
      display: none;
    }
  }
  @keyframes rise {
    from {
      opacity: 0;
      transform: translateY(6px);
    }
  }
</style>
