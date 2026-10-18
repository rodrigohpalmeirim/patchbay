<!--
  Port forwards tab. With a host: that host's rules plus remote rules that
  reach their target from it (machine-to-machine). Without (hostId null): every
  rule, grouped by host.
-->
<script lang="ts">
  import Cable from "@lucide/svelte/icons/cable";
  import Globe from "@lucide/svelte/icons/globe";
  import Laptop from "@lucide/svelte/icons/laptop";
  import Plus from "@lucide/svelte/icons/plus";
  import Search from "@lucide/svelte/icons/search";
  import Server from "@lucide/svelte/icons/server";
  import X from "@lucide/svelte/icons/x";
  import StatusDot from "../components/StatusDot.svelte";
  import ForwardRow from "../forwards/ForwardRow.svelte";
  import { describeRoute, newForward, openForwards } from "../forwards/ops";
  import { connections, STATUS_LABEL } from "../stores/connections.svelte";
  import { forwards, isRunning } from "../stores/forwards.svelte";
  import { hosts } from "../stores/hosts.svelte";
  import type { ForwardsTab } from "../stores/tabs.svelte";
  import type { Host, PortForward } from "../types";
  import { address, collator, plural } from "../util/format";
  import { connectHost } from "../actions";

  let { host }: { tab: ForwardsTab; host: Host | null; active: boolean } = $props();

  let filter = $state("");
  let filterEl = $state<HTMLInputElement>();

  const q = $derived(filter.trim().toLowerCase());
  const matches = (f: PortForward) => {
    if (!q) return true;
    const hay = [f.name, describeRoute(f).text, hosts.get(f.hostId)?.name ?? "", hosts.get(f.viaHostId)?.name ?? ""].join(" ").toLowerCase();
    return hay.includes(q);
  };

  const scope = $derived(host ? forwards.sorted.filter((f) => f.hostId === host.id || f.viaHostId === host.id) : forwards.sorted);
  const own = $derived(host ? forwards.onHost(host.id).filter(matches) : []);
  const via = $derived(host ? forwards.viaHost(host.id).filter(matches) : []);

  /** Global view: rules grouped by carrying host, hosts by name. */
  const groups = $derived.by(() => {
    if (host) return [];
    const by = new Map<string, PortForward[]>();
    for (const f of forwards.sorted) if (matches(f)) by.set(f.hostId, [...(by.get(f.hostId) ?? []), f]);
    return [...by.entries()]
      .map(([id, rules]) => ({ id, host: hosts.get(id), rules }))
      .sort((a, b) => collator.compare(a.host?.name ?? "￿", b.host?.name ?? "￿"));
  });

  const activeN = $derived(scope.filter((f) => forwards.status(f.id).state === "active").length);
  const troubleN = $derived(
    scope.filter((f) => {
      const st = forwards.status(f.id).state;
      return st === "failed" || st === "reconnecting";
    }).length,
  );
  const shown = $derived(host ? own.length + via.length : groups.reduce((n, g) => n + g.rules.length, 0));

  const conn = $derived(host ? connections.get(host.id) : null);
  const retrying = $derived(own.filter((f) => forwards.status(f.id).state === "reconnecting").length);

  function create() {
    newForward(host ? { hostId: host.id } : undefined);
  }

  function onfilterkey(e: KeyboardEvent) {
    if (e.key === "Escape" && filter) {
      e.preventDefault();
      e.stopPropagation();
      filter = "";
    }
  }

  const runningIn = (rules: PortForward[]) => rules.filter((f) => isRunning(forwards.status(f.id))).length;
</script>

<div class="view">
  <div class="content">
    <header class="head">
      <span class="head-icon"><Cable size={18} /></span>
      <div class="titles">
        <h1>Port forwards</h1>
        <p class="sub">
          {#if host}<span>on <strong>{host.name}</strong></span><span class="dotsep">·</span>{:else}<span>All hosts</span><span class="dotsep">·</span>{/if}
          <span>{plural(scope.length, "rule")}</span>
          {#if activeN}<span class="dotsep">·</span><span class="ok">{activeN} active</span>{/if}
          {#if troubleN}<span class="dotsep">·</span><span class="warn">{troubleN} need attention</span>{/if}
        </p>
      </div>
      <div class="head-actions">
        {#if scope.length > 3}
          <div class="filter">
            <Search size={13} class="f-icon" />
            <input bind:this={filterEl} bind:value={filter} placeholder="Filter" aria-label="Filter port forwards" spellcheck="false" autocomplete="off" onkeydown={onfilterkey} />
            {#if filter}
              <button class="icon-btn sm clear" aria-label="Clear filter" onclick={() => ((filter = ""), filterEl?.focus())}><X size={12} /></button>
            {/if}
          </div>
        {/if}
        <button class="btn btn-primary" onclick={create} data-autofocus-panel><Plus size={14} /> New forward</button>
      </div>
    </header>

    {#if host && conn && conn.phase !== "connected" && own.length > 0}
      <div class="notice" class:warn={retrying > 0}>
        <StatusDot status={retrying > 0 ? "busy" : connections.status(host.id)} size={7} />
        <span>
          {#if retrying > 0}
            Connection to {host.name} lost — reconnecting {retrying === 1 ? "1 forward" : `${retrying} forwards`}…
          {:else if conn.phase === "idle" || conn.phase === "error"}
            Not connected — starting a forward connects to {host.name}.
          {:else}
            {STATUS_LABEL[connections.status(host.id)]} to {host.name}…
          {/if}
        </span>
        {#if conn.phase === "idle" || conn.phase === "error"}
          <button class="btn btn-ghost btn-sm" onclick={() => connectHost(host, { open: false })}>{retrying ? "Reconnect now" : "Connect now"}</button>
        {/if}
      </div>
    {/if}

    {#if !forwards.loaded}
      <div class="skeleton" aria-hidden="true">
        {#each [0, 1, 2] as i (i)}<div class="sk"></div>{/each}
      </div>
    {:else if scope.length === 0}
      <div class="empty">
        <div class="empty-art" aria-hidden="true">
          <span><Laptop size={16} /></span>
          <i></i>
          <span><Server size={16} /></span>
          <i class="dashed"></i>
          <span><Globe size={16} /></span>
        </div>
        <h2>{host ? `No forwards on ${host.name} yet` : "No port forwards yet"}</h2>
        <p>
          Reach a database or web app behind {host ? host.name : "a server"} as if it ran on this computer, publish a local port on the
          server, or browse through it with a SOCKS proxy.
        </p>
        <button class="btn btn-primary btn-lg" onclick={create}><Plus size={15} /> New forward</button>
        <p class="fine">Forwards from <code>~/.ssh/config</code> (LocalForward, RemoteForward, DynamicForward) show up here too.</p>
      </div>
    {:else if shown === 0}
      <div class="empty small">
        <h2>No matches</h2>
        <p>No forward matches “{filter.trim()}”.</p>
        <button class="btn btn-sm" onclick={() => (filter = "")}>Clear filter</button>
      </div>
    {:else if host}
      {#if own.length}
        <section class="group" aria-labelledby="own-{host.id}">
          <div class="group-head">
            <h2 id="own-{host.id}" class="group-title">Through {host.name}</h2>
            <span class="count">{runningIn(own)} of {own.length} on</span>
          </div>
          <ul class="rows">
            {#each own as f (f.id)}<ForwardRow forward={f} showHost={false} />{/each}
          </ul>
        </section>
      {/if}
      {#if via.length}
        <section class="group" aria-labelledby="via-{host.id}">
          <div class="group-head">
            <h2 id="via-{host.id}" class="group-title">Connecting out from {host.name}</h2>
            <span class="hint">Remote forwards on other hosts whose traffic {host.name} delivers</span>
          </div>
          <ul class="rows">
            {#each via as f (f.id)}<ForwardRow forward={f} />{/each}
          </ul>
        </section>
      {/if}
    {:else}
      {#each groups as g (g.id)}
        <section class="group" aria-labelledby="grp-{g.id}">
          <div class="group-head">
            {#if g.host}
              <StatusDot status={connections.status(g.id)} size={7} />
              <span class="sr-only">{STATUS_LABEL[connections.status(g.id)]}:</span>
              <h2 id="grp-{g.id}" class="group-title">
                <button type="button" class="host-link" title="Show only {g.host.name}" onclick={() => openForwards(g.id)}>{g.host.name}</button>
              </h2>
              <span class="addr mono">{address(g.host)}</span>
            {:else}
              <h2 id="grp-{g.id}" class="group-title">Unknown host</h2>
            {/if}
            <span class="count">{runningIn(g.rules)} of {g.rules.length} on</span>
            {#if g.host}
              <button class="icon-btn sm" title="New forward on {g.host.name}" aria-label="New forward on {g.host.name}" onclick={() => newForward({ hostId: g.id })}>
                <Plus size={14} />
              </button>
            {/if}
          </div>
          <ul class="rows">
            {#each g.rules as f (f.id)}<ForwardRow forward={f} />{/each}
          </ul>
        </section>
      {/each}
    {/if}
  </div>
</div>

<style>
  .view {
    height: 100%;
    overflow-y: auto;
    container-type: inline-size;
  }
  .content {
    max-width: 980px;
    margin: 0 auto;
    padding: 24px 28px 48px;
  }
  @container (max-width: 640px) {
    .content {
      padding: 16px 14px 32px;
    }
  }
  .head {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 18px;
  }
  .head-icon {
    display: grid;
    place-items: center;
    flex: none;
    width: 38px;
    height: 38px;
    border-radius: var(--radius-md);
    background: var(--accent-soft);
    color: var(--accent-text);
  }
  .titles {
    flex: 1;
    min-width: 0;
  }
  h1 {
    font-size: var(--text-lg);
    font-weight: var(--weight-semibold);
    letter-spacing: -0.01em;
    line-height: 1.3;
  }
  .sub {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0 6px;
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .sub strong {
    color: var(--text);
    font-weight: var(--weight-medium);
  }
  .dotsep {
    color: var(--text-subtle);
  }
  .ok {
    color: var(--success-text);
  }
  .warn {
    color: var(--warning-text);
  }
  .head-actions {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: none;
  }
  @container (max-width: 520px) {
    .head {
      flex-wrap: wrap;
    }
    .head-actions {
      width: 100%;
    }
    .filter {
      flex: 1;
    }
  }
  .filter {
    position: relative;
    display: flex;
    align-items: center;
  }
  .filter :global(.f-icon) {
    position: absolute;
    left: 8px;
    color: var(--text-subtle);
    pointer-events: none;
  }
  .filter input {
    width: 180px;
    height: var(--control-h);
    padding: 0 26px 0 27px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-input);
    font-size: var(--text-base);
  }
  @container (max-width: 520px) {
    .filter input {
      width: 100%;
    }
  }
  .filter input:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .filter .clear {
    position: absolute;
    right: 4px;
  }

  .notice {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: -6px 0 16px;
    padding: 6px 6px 6px 12px;
    border-radius: var(--radius-md);
    background: var(--bg-subtle);
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .notice > span:not(:first-child) {
    flex: 1;
    min-width: 0;
  }
  .notice.warn {
    background: var(--warning-soft);
    color: var(--warning-text);
  }

  .group {
    margin-top: 14px;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-elevated);
    box-shadow: var(--shadow-sm);
    overflow: hidden;
  }
  .group-head {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 40px;
    padding: 6px 14px;
    border-bottom: 1px solid var(--border-subtle);
    background: var(--bg-subtle);
  }
  .group-title {
    flex: none;
    margin: 0;
    font-size: var(--text-base);
    font-weight: var(--weight-semibold);
  }
  .host-link {
    padding: 0;
    border: 0;
    background: none;
    color: inherit;
    font: inherit;
  }
  .host-link:hover {
    color: var(--accent-text);
  }
  .addr {
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--text-subtle);
    font-size: 11.5px;
  }
  .count {
    flex: none;
    color: var(--text-muted);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
  }
  .group-head .count {
    margin-left: auto;
  }
  .group-head .icon-btn {
    margin-right: -6px;
  }
  .hint {
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--text-subtle);
    font-size: var(--text-sm);
  }
  .rows {
    margin: 0;
    padding: 0;
  }
  .rows > :global(li) {
    border-radius: 0;
  }
  .rows > :global(li + li) {
    border-top: 1px solid var(--border-subtle);
  }

  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    max-width: 460px;
    margin: 48px auto 0;
    text-align: center;
  }
  .empty.small {
    margin-top: 32px;
  }
  .empty h2 {
    font-size: var(--text-md);
    font-weight: var(--weight-semibold);
  }
  .empty p {
    margin: 6px 0 18px;
    color: var(--text-muted);
  }
  .empty p.fine {
    margin: 18px 0 0;
    color: var(--text-subtle);
    font-size: var(--text-sm);
  }
  .empty-art {
    display: flex;
    align-items: center;
    margin-bottom: 18px;
    color: var(--text-muted);
  }
  .empty-art span {
    display: grid;
    place-items: center;
    width: 36px;
    height: 36px;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    box-shadow: var(--shadow-sm);
  }
  .empty-art i {
    width: 34px;
    height: 4px;
    margin: 0 4px;
    border-radius: 2px;
    background: var(--accent-soft);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 35%, transparent);
  }
  .empty-art i.dashed {
    height: 0;
    border-top: 1.5px dashed var(--text-subtle);
    background: none;
    box-shadow: none;
  }
  .skeleton {
    display: flex;
    flex-direction: column;
    gap: 10px;
    margin-top: 14px;
  }
  .sk {
    height: 64px;
    border-radius: var(--radius-lg);
    background: var(--bg-subtle);
  }
</style>
