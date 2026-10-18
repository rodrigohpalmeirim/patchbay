<!-- Read-only definition list of a host's settings (overview + ssh_config viewer). -->
<script lang="ts">
  import ArrowRight from "@lucide/svelte/icons/arrow-right";
  import { tabs } from "../stores/tabs.svelte";
  import { hosts } from "../stores/hosts.svelte";
  import type { Host } from "../types";
  import { jumpHops, tildify } from "../util/format";

  let { host, linkHops = true }: { host: Host; linkHops?: boolean } = $props();

  const hops = $derived(jumpHops(host.proxyJump));
</script>

<dl class="details">
  <div class="item">
    <dt>Hostname</dt>
    <dd class="mono selectable">{host.hostname}</dd>
  </div>
  <div class="item">
    <dt>Port</dt>
    <dd class="mono">{host.port}</dd>
  </div>
  <div class="item">
    <dt>User</dt>
    <dd class:none={!host.user}>
      {#if host.user}<span class="mono selectable">{host.user}</span>{:else}Default (local user){/if}
    </dd>
  </div>
  <div class="item">
    <dt>Proxy jump</dt>
    <dd class:none={!hops.length}>
      {#if hops.length}
        <span class="hops">
          {#each hops as hop, i (i)}
            {@const target = hosts.findByName(hop)}
            {#if i > 0}<ArrowRight size={12} class="hop-arrow" />{/if}
            {#if target && linkHops}
              <button class="hop link" onclick={() => tabs.openHost(target.id)} title="Open {target.name}">
                {hop}
              </button>
            {:else}
              <span class="hop mono">{hop}</span>
            {/if}
          {/each}
        </span>
      {:else}
        Direct connection
      {/if}
    </dd>
  </div>
  <div class="item">
    <dt>Identity files</dt>
    <dd class:none={!host.identityFiles.length}>
      {#if host.identityFiles.length}
        <ul class="files">
          {#each host.identityFiles as file (file)}<li class="mono selectable" title={file}>{tildify(file)}</li>{/each}
        </ul>
      {:else}
        SSH agent and default keys
      {/if}
    </dd>
  </div>
  {#if host.source === "app"}
    <div class="item">
      <dt>Group</dt>
      <dd class:none={!host.group}>{host.group ?? "None"}</dd>
    </div>
    <div class="item">
      <dt>Tags</dt>
      <dd class:none={!host.tags.length}>
        {#if host.tags.length}
          <span class="tags">{#each host.tags as tag (tag)}<span class="badge">{tag}</span>{/each}</span>
        {:else}
          None
        {/if}
      </dd>
    </div>
  {/if}
</dl>

<style>
  .details {
    container-type: inline-size;
    margin: 0;
    padding: 2px 16px;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-elevated);
  }
  .item {
    display: grid;
    grid-template-columns: 140px minmax(0, 1fr);
    align-items: baseline;
    gap: 2px 16px;
    min-height: 40px;
    padding: 10px 0;
  }
  .item + .item {
    border-top: 1px solid var(--border-subtle);
  }
  @container (max-width: 420px) {
    .item {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  dt {
    color: var(--text-muted);
  }
  dd {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  dd.none {
    color: var(--text-subtle);
  }
  .files {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .files li + li {
    margin-top: 4px;
  }
  .hops {
    display: inline-flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px;
  }
  .hops :global(.hop-arrow) {
    color: var(--text-subtle);
  }
  .hop.link {
    padding: 0 6px;
    height: 22px;
    border: 1px solid var(--border);
    border-radius: var(--radius-xs);
    background: var(--bg-elevated);
    color: var(--accent-text);
    font-family: var(--font-mono);
    font-size: 12px;
  }
  .hop.link:hover {
    border-color: var(--accent);
    background: var(--accent-soft);
  }
  .tags {
    display: inline-flex;
    flex-wrap: wrap;
    gap: 4px;
  }
</style>
