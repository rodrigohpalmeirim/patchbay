<!--
  One port forward rule: on/off switch, name + route, live state, traffic and
  actions. Used by the forwards views and (compact) the host overview.
-->
<script lang="ts">
  import ArrowDown from "@lucide/svelte/icons/arrow-down";
  import ArrowRight from "@lucide/svelte/icons/arrow-right";
  import ArrowUp from "@lucide/svelte/icons/arrow-up";
  import Copy from "@lucide/svelte/icons/copy";
  import Ellipsis from "@lucide/svelte/icons/ellipsis";
  import ExternalLink from "@lucide/svelte/icons/external-link";
  import FileText from "@lucide/svelte/icons/file-text";
  import Zap from "@lucide/svelte/icons/zap";
  import StatusDot from "../components/StatusDot.svelte";
  import Switch from "../components/Switch.svelte";
  import type { Status } from "../stores/connections.svelte";
  import { connections } from "../stores/connections.svelte";
  import { forwards, isRunning } from "../stores/forwards.svelte";
  import { prompts } from "../stores/prompts.svelte";
  import type { PortForward } from "../types";
  import { sentence } from "../util/errors";
  import { formatBytes, formatSpeed } from "../util/format";
  import { browserUrl, copyAddress, describeRoute, editForward, KIND_FLAG, KIND_LABEL, machineName, openInBrowser, showForwardMenu } from "./ops";

  let {
    forward: f,
    compact = false,
    showHost = true,
  }: {
    forward: PortForward;
    /** Overview summary: no traffic column, fewer actions. */
    compact?: boolean;
    /** Offer "Open <host>" in the menu (off inside that host's own view). */
    showHost?: boolean;
  } = $props();

  const s = $derived(forwards.status(f.id));
  const rate = $derived(forwards.rate(f.id));
  const route = $derived(describeRoute(f));
  const running = $derived(isRunning(s));
  const pending = $derived(!!forwards.pending[f.id]);
  const url = $derived(browserUrl(f, s));

  const dot = $derived.by((): Status => {
    switch (s.state) {
      case "active":
        return "on";
      case "starting":
      case "reconnecting":
        return "busy";
      case "failed":
        return "error";
      default:
        return "off";
    }
  });

  /** While starting, say what the host connection is doing. */
  const startingLabel = $derived.by(() => {
    const ids = f.kind === "remote" && f.viaHostId ? [f.hostId, f.viaHostId] : [f.hostId];
    for (const id of ids) {
      if (prompts.isWaiting(id)) return `Waiting for your input (${machineName(id)})…`;
      const phase = connections.get(id).phase;
      if (phase === "connecting" || phase === "authenticating") return `Connecting to ${machineName(id)}…`;
    }
    return "Starting…";
  });

  const listenOn = $derived(s.state === "active" ? (f.kind === "remote" ? `${s.address} on ${machineName(f.hostId)}` : s.address) : "");

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "ContextMenu" || (e.key === "F10" && e.shiftKey)) {
      e.preventDefault();
      showForwardMenu(f, e.currentTarget as HTMLElement, { showHost });
    }
  }

  function oncontextmenu(e: MouseEvent) {
    e.preventDefault();
    showForwardMenu(f, { x: e.clientX, y: e.clientY }, { showHost });
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<li class="fwd" class:compact class:running class:failed={s.state === "failed"} {oncontextmenu} {onkeydown} aria-label="{f.name}: {route.text}">
  <div class="toggle">
    <Switch
      checked={running}
      busy={pending || s.state === "starting"}
      tone={s.state === "reconnecting" ? "warning" : "accent"}
      label="{running ? 'Stop' : 'Start'} {f.name}"
      title={running ? "Stop forwarding" : "Start forwarding"}
      onchange={(on) => (on ? void forwards.start(f.id) : void forwards.stop(f.id))}
    />
  </div>

  <div class="main">
    <div class="line1">
      {#if f.source === "app" && !compact}
        <button type="button" class="name linkish" title="Edit “{f.name}”" onclick={() => editForward(f)}>{f.name}</button>
      {:else}
        <span class="name">{f.name}</span>
      {/if}
      <span class="kind" title="{KIND_LABEL[f.kind]} forward (ssh {KIND_FLAG[f.kind]})">{f.kind === "dynamic" ? "SOCKS" : KIND_LABEL[f.kind]}</span>
      {#if f.source === "sshConfig"}
        <span class="tag" title="Defined in ~/.ssh/config (read-only)"><FileText size={10} /> SSH config</span>
      {/if}
      {#if f.autoStart}
        <span class="tag auto" title="Starts automatically when Patchbay launches"><Zap size={10} /> Auto-start</span>
      {/if}
    </div>
    <div class="route mono" title={route.text}>
      <span class="ep">{route.from}</span>
      {#if route.to}
        <ArrowRight size={12} class="arrow" aria-hidden="true" />
        <span class="ep">{route.to}</span>
      {/if}
      {#if route.viaLabel}<span class="via">{route.viaLabel}</span>{/if}
    </div>
    <div class="state {s.state}" aria-live="polite">
      <StatusDot status={dot} size={7} />
      {#if s.state === "active"}
        <span>Listening on <span class="mono selectable">{listenOn}</span></span>
        {#if compact}
          <span class="mini" title="{s.openConnections} open connections · received {formatBytes(s.bytesIn)}, sent {formatBytes(s.bytesOut)}">
            · {s.openConnections} {s.openConnections === 1 ? "connection" : "connections"}{rate.in + rate.out >= 1 ? ` · ${formatSpeed(rate.in + rate.out)}` : ""}
          </span>
        {/if}
      {:else if s.state === "starting"}
        <span>{startingLabel}</span>
      {:else if s.state === "reconnecting"}
        <span class="strong lead">Reconnecting · attempt {s.attempt}</span>
        {#if s.message}<span class="msg" title={sentence(s.message)}>{sentence(s.message)}</span>{/if}
      {:else if s.state === "failed"}
        <span class="strong lead">Failed</span>
        <span class="msg selectable" title={sentence(s.message)}>{sentence(s.message)}</span>
      {:else}
        <span>Stopped</span>
      {/if}
    </div>
  </div>

  {#if !compact}
    <div class="stats" class:idle={s.state !== "active"}>
      {#if s.state === "active"}
        <span class="conns" title="{s.openConnections} open, {s.totalConnections} since start">
          <strong>{s.openConnections}</strong>
          {s.openConnections === 1 ? "connection" : "connections"}
        </span>
        <span class="traffic" title="Received {formatBytes(s.bytesIn)}">
          <ArrowDown size={11} class="dir in" aria-label="In" />
          <span class="amount">{formatBytes(s.bytesIn)}</span>
          <span class="speed">{rate.in >= 1 ? formatSpeed(rate.in) : "idle"}</span>
        </span>
        <span class="traffic" title="Sent {formatBytes(s.bytesOut)}">
          <ArrowUp size={11} class="dir out" aria-label="Out" />
          <span class="amount">{formatBytes(s.bytesOut)}</span>
          <span class="speed">{rate.out >= 1 ? formatSpeed(rate.out) : "idle"}</span>
        </span>
      {/if}
    </div>
  {/if}

  <div class="actions">
    {#if url}
      <button class="icon-btn" title="Open {url} in browser" aria-label="Open {f.name} in browser" onclick={() => void openInBrowser(url)}>
        <ExternalLink size={14} />
      </button>
    {/if}
    {#if !compact}
      <button class="icon-btn copy" title="Copy address" aria-label="Copy address of {f.name}" onclick={() => void copyAddress(f)}>
        <Copy size={14} />
      </button>
    {/if}
    <button
      class="icon-btn"
      title="More actions"
      aria-label="More actions for {f.name}"
      aria-haspopup="menu"
      onclick={(e) => showForwardMenu(f, e.currentTarget, { showHost })}
    >
      <Ellipsis size={15} />
    </button>
  </div>
</li>

<style>
  .fwd {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto auto;
    grid-template-areas: "toggle main stats actions";
    align-items: center;
    column-gap: 14px;
    padding: 11px 10px 11px 14px;
    list-style: none;
    border-radius: var(--radius-md);
    transition: background var(--dur-fast);
  }
  .fwd:hover {
    background: var(--bg-hover);
  }
  .toggle {
    grid-area: toggle;
    display: flex;
    align-self: start;
    padding-top: 2px;
  }
  .main {
    grid-area: main;
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .line1 {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .name {
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-weight: var(--weight-semibold);
  }
  .linkish {
    padding: 0;
    border: 0;
    background: none;
    color: inherit;
    font: inherit;
    font-weight: var(--weight-semibold);
    text-align: left;
  }
  .linkish:hover {
    text-decoration: underline;
    text-decoration-color: var(--text-subtle);
    text-underline-offset: 2px;
  }
  .kind,
  .tag {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    flex: none;
    height: 17px;
    padding: 0 6px;
    border-radius: var(--radius-full);
    font-size: 10.5px;
    font-weight: var(--weight-medium);
    white-space: nowrap;
  }
  .kind {
    border: 1px solid var(--border);
    color: var(--text-muted);
  }
  .tag {
    background: var(--bg-active);
    color: var(--text-muted);
  }
  .tag.auto {
    background: var(--accent-soft);
    color: var(--accent-text);
  }
  .route {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0 6px;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    color: var(--text);
    font-size: 12px;
  }
  .ep {
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    flex: 0 1 auto;
  }
  .route :global(.arrow) {
    flex: none;
    color: var(--text-subtle);
  }
  .via {
    flex: 0 1 auto;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--text-muted);
    font-family: var(--font-sans);
    font-size: var(--text-sm);
  }
  .state {
    display: flex;
    align-items: baseline;
    gap: 7px;
    min-width: 0;
    margin-top: 1px;
    color: var(--text-muted);
    font-size: var(--text-sm);
    white-space: nowrap;
  }
  .state > :global(.dot) {
    align-self: center;
  }
  .state.failed > :global(.dot),
  .state.reconnecting > :global(.dot) {
    align-self: flex-start;
    margin-top: 6px;
  }
  .state > span {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .state > .lead {
    flex: none;
  }
  .state .mono {
    font-size: 11.5px;
    color: var(--text);
  }
  .state.reconnecting .strong {
    color: var(--warning-text);
    font-weight: var(--weight-medium);
  }
  .state.failed .strong {
    color: var(--danger-text);
    font-weight: var(--weight-medium);
  }
  .msg {
    flex: 0 1 auto;
    white-space: normal;
    overflow-wrap: anywhere;
  }
  .mini {
    color: var(--text-subtle);
    font-variant-numeric: tabular-nums;
  }
  .state.failed .msg {
    color: var(--text);
  }

  .stats {
    grid-area: stats;
    display: grid;
    grid-template-columns: auto;
    justify-items: end;
    gap: 1px;
    min-width: 150px;
    color: var(--text-muted);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .stats.idle {
    min-width: 0;
  }
  .conns strong {
    color: var(--text);
    font-weight: var(--weight-semibold);
  }
  .traffic {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .traffic :global(.dir) {
    flex: none;
  }
  .traffic :global(.dir.in) {
    color: var(--success-text);
  }
  .traffic :global(.dir.out) {
    color: var(--accent-text);
  }
  .amount {
    color: var(--text);
  }
  .speed {
    min-width: 64px;
    color: var(--text-subtle);
    text-align: right;
  }

  .actions {
    grid-area: actions;
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 1px;
    min-width: 80px;
    opacity: 0.55;
    transition: opacity var(--dur-fast);
  }
  .fwd:hover .actions,
  .fwd:focus-within .actions {
    opacity: 1;
  }

  /* Narrow: traffic moves under the route. */
  @container (max-width: 600px) {
    .fwd {
      grid-template-columns: auto minmax(0, 1fr) auto;
      grid-template-areas:
        "toggle main actions"
        "toggle stats stats";
    }
    .stats {
      display: flex;
      flex-wrap: wrap;
      justify-items: start;
      gap: 2px 14px;
      min-width: 0;
      margin-top: 4px;
    }
    .stats.idle {
      display: none;
    }
    .speed {
      min-width: 0;
      text-align: left;
    }
  }
  @container (max-width: 480px) {
    .copy {
      display: none;
    }
    .actions {
      min-width: 0;
    }
  }

  .compact {
    grid-template-columns: auto minmax(0, 1fr) auto;
    grid-template-areas: "toggle main actions";
    padding: 9px 8px 9px 12px;
  }
  .compact .actions {
    min-width: 0;
  }
</style>
