<script lang="ts">
  import Cable from "@lucide/svelte/icons/cable";
  import Check from "@lucide/svelte/icons/check";
  import CircleAlert from "@lucide/svelte/icons/circle-alert";
  import Copy from "@lucide/svelte/icons/copy";
  import Ellipsis from "@lucide/svelte/icons/ellipsis";
  import FileText from "@lucide/svelte/icons/file-text";
  import FolderOpen from "@lucide/svelte/icons/folder-open";
  import House from "@lucide/svelte/icons/house";
  import LoaderCircle from "@lucide/svelte/icons/loader-circle";
  import Pencil from "@lucide/svelte/icons/pencil";
  import Plug from "@lucide/svelte/icons/plug";
  import Plus from "@lucide/svelte/icons/plus";
  import RotateCw from "@lucide/svelte/icons/rotate-cw";
  import Server from "@lucide/svelte/icons/server";
  import SquareTerminal from "@lucide/svelte/icons/square-terminal";
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";
  import Unplug from "@lucide/svelte/icons/unplug";
  import X from "@lucide/svelte/icons/x";
  import ArrowRight from "@lucide/svelte/icons/arrow-right";
  import { connectHost, copySshCommand, disconnectHost, editHost, hostMenuItems } from "../actions";
  import { openFiles } from "../files/ops";
  import ForwardRow from "../forwards/ForwardRow.svelte";
  import { newForward } from "../forwards/ops";
  import { forwards } from "../stores/forwards.svelte";
  import { tabs } from "../stores/tabs.svelte";
  import { plural } from "../util/format";
  import { NEW_TERMINAL_KEY } from "../terminal/keys";
  import { openTerminal } from "../terminal/ops";
  import OsIcon from "../components/OsIcon.svelte";
  import HostDetails from "../components/HostDetails.svelte";
  import StatusDot from "../components/StatusDot.svelte";
  import { connections } from "../stores/connections.svelte";
  import { prompts } from "../stores/prompts.svelte";
  import type { HostTab } from "../stores/tabs.svelte";
  import { ui } from "../stores/ui.svelte";
  import type { Host } from "../types";
  import { errorTitle, sentence } from "../util/errors";
  import { address, hopLabel, jumpHops, osLabel } from "../util/format";

  let { host }: { tab: HostTab; host: Host; active: boolean } = $props();

  const conn = $derived(connections.get(host.id));
  const status = $derived(connections.status(host.id));
  const phase = $derived(conn.phase);
  const busy = $derived(status === "busy");
  const waiting = $derived(prompts.isWaiting(host.id));
  const caps = $derived(conn.capabilities);

  type StepState = "done" | "active" | "error" | "pending";
  interface Step {
    key: string;
    label: string;
    detail?: string;
    state: StepState;
  }

  const steps = $derived.by((): Step[] => {
    const expected = jumpHops(host.proxyJump);
    const hops = [...conn.hopsSeen, ...expected.filter((h) => !conn.hopsSeen.includes(h))];
    const raw: Omit<Step, "state">[] = [
      ...hops.map((h) => ({ key: `hop:${h}`, label: `Via ${hopLabel(h)}`, detail: "Jump host" })),
      {
        key: "connect",
        label: `Connecting to ${host.hostname}`,
        detail: host.port !== 22 ? `Port ${host.port}` : undefined,
      },
      {
        key: "auth",
        label: "Authenticating",
      },
      { key: "ready", label: "Connected" },
    ];
    let current = raw.findIndex((s) => s.key === "connect");
    if (conn.stage === "connecting" && conn.hop) {
      const i = raw.findIndex((s) => s.key === `hop:${conn.hop}`);
      if (i !== -1) current = i;
    } else if (conn.stage === "authenticating") current = raw.length - 2;
    else if (conn.stage === "connected") current = raw.length - 1;
    return raw.map((s, i) => {
      let state: StepState = "pending";
      if (i < current) state = "done";
      else if (i === current) state = phase === "error" ? "error" : "active";
      if (i === current && waiting && state === "active") s.detail = "Waiting for your input…";
      return { ...s, state };
    });
  });

  const hostRules = $derived(forwards.sorted.filter((f) => f.hostId === host.id || f.viaHostId === host.id));
  /** Rules worth showing here: running, or failed and waiting for attention. */
  const liveRules = $derived(hostRules.filter((f) => forwards.status(f.id).state !== "stopped"));
  const activeRules = $derived(hostRules.filter((f) => forwards.status(f.id).state === "active").length);
  const noShell = $derived(!!caps && !caps.exec);

  function openMenu(e: MouseEvent) {
    ui.openMenu(`${host.name} actions`, hostMenuItems(host, { includeOpen: false }), e.currentTarget as HTMLElement);
  }
</script>

<div class="overview">
  <div class="content">
    <header class="hero">
      <div class="avatar" class:on={phase === "connected"}>
        {#if caps && caps.os !== "unknown"}
          <OsIcon os={caps.os} size={22} />
        {:else}
          <Server size={22} />
        {/if}
        <span class="avatar-dot"><StatusDot {status} size={10} /></span>
      </div>
      <div class="titles">
        <div class="title-row">
          <h1 class="selectable" title={host.name}>{host.name}</h1>
        </div>
        <div class="address">
          {#if host.source === "sshConfig"}
            <span class="badge" title="Defined in ~/.ssh/config (read-only)"><FileText size={11} /> SSH config</span>
          {:else}
            <span class="badge accent" title="Saved in Patchbay">Patchbay</span>
          {/if}
          <span class="mono selectable">{address(host)}</span>
          <button class="icon-btn sm" title="Copy SSH command" aria-label="Copy SSH command" onclick={() => copySshCommand(host)}>
            <Copy size={13} />
          </button>
        </div>
      </div>
      <div class="hero-actions">
        {#if phase === "connected"}
          <button class="btn btn-lg" onclick={() => void disconnectHost(host)}><Unplug size={15} /> Disconnect</button>
        {:else if phase === "disconnecting"}
          <button class="btn btn-lg" disabled><LoaderCircle size={15} class="spin" /> Disconnecting</button>
        {:else if busy}
          <button class="btn btn-lg" onclick={() => void disconnectHost(host)}><X size={15} /> Cancel</button>
        {:else}
          <button class="btn btn-primary btn-lg" onclick={() => connectHost(host)}>
            {#if phase === "error"}<RotateCw size={15} /> Retry{:else}<Plug size={15} /> Connect{/if}
          </button>
        {/if}
        <button class="icon-btn more" aria-label="More actions" aria-haspopup="menu" title="More actions" onclick={openMenu}>
          <Ellipsis size={16} />
        </button>
      </div>
    </header>

    <section class="card connection" aria-labelledby="conn-{host.id}" aria-live="polite">
      <h2 id="conn-{host.id}" class="sr-only">Connection</h2>
      {#if phase === "connected" && caps}
        <div class="conn-head">
          <StatusDot status="on" size={8} />
          <span class="conn-title">Connected</span>
          {#if conn.connectedAt}
            <span class="muted">since {new Date(conn.connectedAt).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</span>
          {/if}
        </div>
        <div class="caps">
          <div class="cap">
            <span class="cap-icon"><OsIcon os={caps.os} size={16} /></span>
            <span class="cap-label">System</span>
            <span class="cap-value">{osLabel(caps.os)}</span>
          </div>
          <div class="cap">
            <span class="cap-icon"><FolderOpen size={16} /></span>
            <span class="cap-label">File transfer</span>
            <span class="cap-value" class:no={!caps.sftp}>{caps.sftp ? "SFTP" : "Not available"}</span>
          </div>
          <div class="cap">
            <span class="cap-icon"><SquareTerminal size={16} /></span>
            <span class="cap-label">Shell</span>
            <span class="cap-value" class:no={!caps.exec}>{caps.exec ? "Available" : "SFTP only"}</span>
          </div>
          <div class="cap home">
            <span class="cap-icon"><House size={16} /></span>
            <span class="cap-label">Home directory</span>
            <span class="cap-value mono selectable" title={caps.home ?? undefined}>{caps.home ?? "Unknown"}</span>
          </div>
        </div>
      {:else if busy || phase === "error"}
        {#if phase === "reconnecting"}
          <div class="conn-head">
            <LoaderCircle size={15} class="spin warn-icon" />
            <span class="conn-title">Reconnecting…</span>
            <span class="muted">attempt {conn.attempt}</span>
          </div>
        {:else if phase === "disconnecting"}
          <div class="conn-head">
            <LoaderCircle size={15} class="spin" />
            <span class="conn-title">Disconnecting…</span>
          </div>
        {:else}
          <ol class="steps" aria-label="Connection progress">
            {#each steps as step (step.key)}
              <li class="step {step.state}" aria-current={step.state === "active" ? "step" : undefined}>
                <span class="step-mark" aria-hidden="true">
                  {#if step.state === "done"}
                    <Check size={12} strokeWidth={3} />
                  {:else if step.state === "active"}
                    <LoaderCircle size={14} class="spin" />
                  {:else if step.state === "error"}
                    <X size={12} strokeWidth={3} />
                  {/if}
                </span>
                <span class="step-text">
                  <span class="step-label">{step.label}</span>
                  {#if step.detail}<span class="step-detail" class:waiting={step.detail.startsWith("Waiting")}>{step.detail}</span>{/if}
                </span>
                <span class="sr-only">
                  ({step.state === "done" ? "done" : step.state === "active" ? "in progress" : step.state === "error" ? "failed" : "pending"})
                </span>
              </li>
            {/each}
          </ol>
        {/if}
        {#if phase === "error" && conn.error}
          <div class="callout danger" role="alert">
            <CircleAlert size={16} />
            <div class="callout-body">
              <div class="callout-title">{errorTitle(conn.error.kind)}</div>
              <div class="callout-text selectable">{sentence(conn.error.message)}</div>
              <div class="callout-actions">
                <button class="btn btn-sm" onclick={() => editHost(host)}>
                  {host.source === "app" ? "Edit host…" : "View settings…"}
                </button>
                <span class="spacer"></span>
                <button class="icon-btn sm" aria-label="Dismiss error" title="Dismiss" onclick={() => connections.dismiss(host.id)}>
                  <X size={13} />
                </button>
              </div>
            </div>
          </div>
        {/if}
      {:else}
        {#if conn.lostReason}
          <div class="callout warning">
            <TriangleAlert size={16} />
            <div class="callout-body">
              <div class="callout-title">Connection lost</div>
              <div class="callout-text selectable">{sentence(conn.lostReason)}</div>
            </div>
          </div>
        {/if}
        <div class="idle">
          <span class="idle-icon"><Plug size={18} /></span>
          <div>
            <div class="conn-title">Not connected</div>
            <div class="muted">Connect to browse files, open a terminal or forward ports.</div>
          </div>
        </div>
      {/if}
    </section>

    <section class="block" aria-labelledby="actions-{host.id}">
      <h2 id="actions-{host.id}" class="block-title">Open</h2>
      <div class="actions">
        <button
          type="button"
          class="action live"
          disabled={!!caps && !caps.sftp}
          onclick={() => openFiles(host.id, { reuse: true })}
        >
          <div class="action-top">
            <span class="action-icon"><FolderOpen size={18} /></span>
            <span class="go" aria-hidden="true"><ArrowRight size={15} /></span>
          </div>
          <span class="action-title">Browse files</span>
          <span class="action-desc">
            {#if caps && !caps.sftp}
              SFTP is not available on this server
            {:else if phase === "connected"}
              Browse, upload and download over SFTP.
            {:else}
              Connects and opens the file browser.
            {/if}
          </span>
        </button>
        <button
          type="button"
          class="action live"
          disabled={noShell}
          title={noShell ? undefined : `Open terminal (${NEW_TERMINAL_KEY})`}
          onclick={() => openTerminal(host.id)}
        >
          <div class="action-top">
            <span class="action-icon"><SquareTerminal size={18} /></span>
            <span class="go" aria-hidden="true"><ArrowRight size={15} /></span>
          </div>
          <span class="action-title">Open terminal</span>
          <span class="action-desc">
            {#if noShell}
              This server only allows SFTP, so there’s no shell.
            {:else if phase === "connected"}
              Interactive shell on {host.name}.
            {:else}
              Connects and opens a shell.
            {/if}
          </span>
        </button>
        <button type="button" class="action live" onclick={() => tabs.openForwards(host.id)}>
          <div class="action-top">
            <span class="action-icon"><Cable size={18} /></span>
            {#if activeRules > 0}
              <span class="live-pill"><span class="live-dot"></span>{activeRules} active</span>
            {:else}
              <span class="go" aria-hidden="true"><ArrowRight size={15} /></span>
            {/if}
          </div>
          <span class="action-title">Port forwards</span>
          <span class="action-desc">
            {#if hostRules.length}
              {plural(hostRules.length, "rule")} · local, remote and SOCKS tunnels.
            {:else}
              Local, remote and SOCKS tunnels.
            {/if}
          </span>
        </button>
      </div>
    </section>

    {#if liveRules.length}
      <section class="block" aria-labelledby="fwd-{host.id}">
        <div class="block-head">
          <h2 id="fwd-{host.id}" class="block-title">Running forwards</h2>
          <div class="block-actions">
            <button class="btn btn-ghost btn-sm" onclick={() => newForward({ hostId: host.id })}><Plus size={13} /> New forward</button>
            <button class="btn btn-ghost btn-sm" onclick={() => tabs.openForwards(host.id)}>Manage <ArrowRight size={13} /></button>
          </div>
        </div>
        <ul class="fwd-list">
          {#each liveRules as f (f.id)}<ForwardRow forward={f} compact showHost={f.hostId !== host.id} />{/each}
        </ul>
      </section>
    {/if}

    <section class="block" aria-labelledby="details-{host.id}">
      <div class="block-head">
        <h2 id="details-{host.id}" class="block-title">Details</h2>
        {#if host.source === "app"}
          <button class="btn btn-ghost btn-sm" onclick={() => editHost(host)}><Pencil size={13} /> Edit</button>
        {:else}
          <span class="muted small">From <code>~/.ssh/config</code> · read-only</span>
        {/if}
      </div>
      <HostDetails {host} />
    </section>
  </div>
</div>

<style>
  .overview {
    height: 100%;
    overflow-y: auto;
    container-type: inline-size;
  }
  @container (max-width: 560px) {
    .hero {
      flex-wrap: wrap;
    }
    .hero-actions {
      order: 3;
      width: 100%;
    }
    .hero-actions .btn {
      flex: 1;
    }
  }
  .content {
    max-width: 860px;
    margin: 0 auto;
    padding: 28px 32px 48px;
  }
  @container (max-width: 640px) {
    .content {
      padding: 20px 20px 40px;
    }
  }

  /* -------- hero */
  .hero {
    display: flex;
    align-items: center;
    gap: 14px;
    margin-bottom: 20px;
  }
  .avatar {
    position: relative;
    display: grid;
    place-items: center;
    flex: none;
    width: 48px;
    height: 48px;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-subtle);
    color: var(--text-muted);
  }
  .avatar.on {
    color: var(--text);
  }
  .avatar-dot {
    position: absolute;
    right: -3px;
    bottom: -3px;
    display: flex;
    padding: 2px;
    border-radius: 50%;
    background: var(--bg);
  }
  .titles {
    flex: 1;
    min-width: 0;
  }
  .title-row {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  h1 {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: var(--text-xl);
    font-weight: var(--weight-semibold);
    letter-spacing: -0.015em;
    line-height: 1.25;
  }
  .address {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 4px;
    color: var(--text-muted);
    min-width: 0;
  }
  .address .mono {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .address .icon-btn {
    margin-left: -4px;
    opacity: 0;
  }
  .hero:hover .address .icon-btn,
  .address .icon-btn:focus-visible {
    opacity: 1;
  }
  .hero-actions {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: none;
  }
  .more {
    width: 34px;
    height: 34px;
  }

  /* -------- connection card */
  .card {
    padding: 16px 18px;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-elevated);
    box-shadow: var(--shadow-sm);
  }
  .conn-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .conn-head :global(.warn-icon) {
    color: var(--warning);
  }
  .conn-title {
    font-weight: var(--weight-semibold);
  }
  .muted {
    color: var(--text-muted);
  }
  .small {
    font-size: var(--text-sm);
  }
  .spacer {
    flex: 1;
  }

  .caps {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-top: 14px;
  }
  .cap {
    flex: 1 1 140px;
  }
  .cap.home {
    flex: 2 1 220px;
  }
  .cap {
    display: grid;
    grid-template-columns: auto 1fr;
    grid-template-rows: auto auto;
    column-gap: 10px;
    row-gap: 1px;
    align-items: center;
    min-width: 0;
    padding: 10px 12px;
    border-radius: var(--radius-md);
    background: var(--bg-subtle);
  }
  .cap-icon {
    grid-row: 1 / 3;
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: var(--radius-sm);
    background: var(--bg-elevated);
    border: 1px solid var(--border-subtle);
    color: var(--text-muted);
  }
  .cap-label {
    color: var(--text-subtle);
    font-size: var(--text-xs);
    font-weight: var(--weight-medium);
  }
  .cap-value {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-weight: var(--weight-medium);
  }
  .cap-value.no {
    color: var(--text-muted);
    font-weight: normal;
  }
  .cap-value.mono {
    font-size: 12px;
  }

  .steps {
    display: flex;
    flex-direction: column;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .step {
    position: relative;
    display: flex;
    align-items: flex-start;
    gap: 12px;
    min-height: 34px;
  }
  .step:not(:last-child)::before {
    content: "";
    position: absolute;
    left: 9.5px;
    top: 22px;
    bottom: -2px;
    width: 1px;
    background: var(--border-strong);
  }
  .step.done:not(:last-child)::before {
    background: var(--success);
    opacity: 0.5;
  }
  .step-mark {
    display: grid;
    place-items: center;
    flex: none;
    width: 20px;
    height: 20px;
    border-radius: 50%;
    border: 1.5px solid var(--border-strong);
    background: var(--bg-elevated);
    color: var(--text-subtle);
  }
  .step.done .step-mark {
    border-color: transparent;
    background: var(--success-soft);
    color: var(--success-text);
  }
  .step.active .step-mark {
    border-color: transparent;
    color: var(--accent-text);
  }
  .step.error .step-mark {
    border-color: transparent;
    background: var(--danger);
    color: #fff;
  }
  .step-text {
    display: flex;
    flex-direction: column;
    padding-top: 1px;
    padding-bottom: 8px;
    min-width: 0;
  }
  .step-label {
    overflow-wrap: anywhere;
  }
  .step.pending .step-label {
    color: var(--text-subtle);
  }
  .step.active .step-label {
    font-weight: var(--weight-medium);
  }
  .step.error .step-label {
    color: var(--danger-text);
    font-weight: var(--weight-medium);
  }
  .step-detail {
    color: var(--text-subtle);
    font-size: var(--text-sm);
  }
  .step-detail.waiting {
    color: var(--warning-text);
  }

  .callout {
    display: flex;
    gap: 10px;
    padding: 12px 12px 12px 14px;
    border-radius: var(--radius-md);
  }
  .callout + .idle {
    margin-top: 14px;
  }
  .steps + .callout {
    margin-top: 10px;
  }
  .callout > :global(svg) {
    flex: none;
    margin-top: 1px;
  }
  .callout.danger {
    background: var(--danger-soft);
    color: var(--danger-text);
  }
  .callout.warning {
    background: var(--warning-soft);
    color: var(--warning-text);
  }
  .callout-body {
    flex: 1;
    min-width: 0;
  }
  .callout-title {
    font-weight: var(--weight-semibold);
  }
  .callout-text {
    margin-top: 2px;
    color: var(--text);
    overflow-wrap: anywhere;
  }
  .callout-actions {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: 10px;
  }

  .idle {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .idle-icon {
    display: grid;
    place-items: center;
    flex: none;
    width: 36px;
    height: 36px;
    border-radius: 50%;
    background: var(--bg-subtle);
    color: var(--text-subtle);
  }

  /* -------- blocks */
  .block {
    margin-top: 28px;
  }
  .block-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 10px;
  }
  .block-title {
    margin-bottom: 10px;
    color: var(--text-muted);
    font-size: var(--text-sm);
    font-weight: var(--weight-semibold);
  }
  .block-head .block-title {
    margin-bottom: 0;
  }

  .actions {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(210px, 1fr));
    gap: 10px;
  }
  .action {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 14px 14px 16px;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-elevated);
  }
  .action-top {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    margin-bottom: 10px;
  }
  .action.live {
    text-align: left;
    color: var(--text);
    font: inherit;
    transition:
      border-color var(--dur-fast),
      box-shadow var(--dur-fast),
      background var(--dur-fast);
  }
  .action.live:hover:not(:disabled) {
    border-color: var(--border-strong);
    box-shadow: var(--shadow-md);
  }
  .action.live:hover:not(:disabled) .go {
    color: var(--accent-text);
    transform: translateX(2px);
  }
  .action.live:disabled {
    opacity: 0.6;
  }
  .go {
    display: flex;
    color: var(--text-subtle);
    transition:
      transform var(--dur-fast) var(--ease-out),
      color var(--dur-fast);
  }
  .action-icon {
    display: grid;
    place-items: center;
    flex: none;
    width: 34px;
    height: 34px;
    border-radius: var(--radius-md);
    background: var(--accent-soft);
    color: var(--accent-text);
  }
  .action-title {
    font-weight: var(--weight-semibold);
  }
  .live-pill {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 2px 8px;
    border-radius: var(--radius-full);
    background: var(--success-soft);
    color: var(--success-text);
    font-size: 10.5px;
    font-weight: var(--weight-medium);
  }
  .live-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--success);
  }
  .block-actions {
    display: flex;
    gap: 2px;
  }
  .fwd-list {
    margin: 0;
    padding: 0;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-elevated);
    overflow: hidden;
    container-type: inline-size;
  }
  .fwd-list > :global(li) {
    border-radius: 0;
  }
  .fwd-list > :global(li + li) {
    border-top: 1px solid var(--border-subtle);
  }
  .action-desc {
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
</style>
