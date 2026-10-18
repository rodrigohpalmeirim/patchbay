<!-- Shown in the main area when no tab is open. -->
<script lang="ts">
  import Plus from "@lucide/svelte/icons/plus";
  import Laptop from "@lucide/svelte/icons/laptop";
  import { newHost } from "../actions";
  import { openFiles } from "../files/ops";
  import Logo from "../components/Logo.svelte";
  import { hosts } from "../stores/hosts.svelte";
  import { ui } from "../stores/ui.svelte";
  import { modKey } from "../util/platform";

  const cfgCount = $derived(hosts.list.filter((h) => h.source === "sshConfig").length);
  const appCount = $derived(hosts.list.length - cfgCount);
</script>

<div class="welcome">
  <div class="inner">
    <Logo size={56} />
    <h1>Welcome to Patchbay</h1>
    {#if hosts.loaded && hosts.list.length === 0}
      <p>Add your first host to get started. Hosts from <code>~/.ssh/config</code> show up automatically.</p>
    {:else}
      <p>
        Pick a host from the sidebar to see its details and connect.
        {#if hosts.loaded}
          <span class="stats">
            {cfgCount} from <code>~/.ssh/config</code> · {appCount} saved in Patchbay
          </span>
        {/if}
      </p>
    {/if}
    <div class="cta">
      <button class="btn btn-primary btn-lg" onclick={() => newHost()}><Plus size={15} /> Add host</button>
      <button class="btn btn-lg" onclick={() => openFiles(null, { reuse: true })}><Laptop size={15} /> Browse this computer</button>
    </div>
    <dl class="shortcuts">
      <div><dt>Search hosts</dt><dd><kbd>/</kbd></dd></div>
      <div><dt>Move through hosts</dt><dd><kbd>↑</kbd><kbd>↓</kbd></dd></div>
      <div><dt>Open selected host</dt><dd><kbd>Enter</kbd></dd></div>
      <div><dt>New host</dt><dd><kbd>{modKey}</kbd><kbd>N</kbd></dd></div>
      <div><dt>Port forwards</dt><dd><kbd>{modKey}</kbd><kbd>Shift</kbd><kbd>P</kbd></dd></div>
      <div>
        <dt><button class="linkish" onclick={() => (ui.settingsOpen = true)}>Settings</button></dt>
        <dd><kbd>{modKey}</kbd><kbd>,</kbd></dd>
      </div>
    </dl>
  </div>
</div>

<style>
  .welcome {
    display: grid;
    place-items: center;
    height: 100%;
    overflow-y: auto;
    padding: 32px;
  }
  .inner {
    display: flex;
    flex-direction: column;
    align-items: center;
    max-width: 420px;
    text-align: center;
  }
  h1 {
    margin-top: 18px;
    font-size: var(--text-xl);
    font-weight: var(--weight-semibold);
    letter-spacing: -0.015em;
  }
  p {
    margin-top: 6px;
    color: var(--text-muted);
  }
  .stats {
    display: block;
    margin-top: 4px;
    color: var(--text-subtle);
    font-size: var(--text-sm);
  }
  .cta {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: 8px;
    margin-top: 20px;
  }
  .shortcuts {
    display: flex;
    flex-direction: column;
    gap: 8px;
    width: 260px;
    margin: 32px 0 0;
    padding-top: 20px;
    border-top: 1px solid var(--border-subtle);
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .shortcuts div {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }
  dt {
    text-align: left;
  }
  dd {
    display: flex;
    gap: 3px;
    margin: 0;
  }
  .linkish {
    padding: 0;
    border: 0;
    background: none;
    color: inherit;
    font-size: inherit;
  }
  .linkish:hover {
    color: var(--accent-text);
  }
</style>
