<!--
  Asks which user to log in as: for Tailscale devices nobody has logged in
  to yet, after a failed login, and for "Connect as…".
-->
<script lang="ts">
  import UserRound from "@lucide/svelte/icons/user-round";
  import { untrack } from "svelte";
  import Dialog from "../components/Dialog.svelte";
  import { connections } from "../stores/connections.svelte";
  import { hosts } from "../stores/hosts.svelte";
  import { ui, type ConnectAsState } from "../stores/ui.svelte";

  let { request }: { request: ConnectAsState } = $props();

  const host = $derived(hosts.get(request.hostId));
  const suggestions = $derived(host?.tailscale?.userSuggestions ?? []);
  let user = $state(untrack(() => request.user ?? host?.user ?? suggestions[0] ?? ""));
  const valid = $derived(/^[^\s@]+$/.test(user.trim()));

  function onsubmit(e: SubmitEvent) {
    e.preventDefault();
    if (!valid) return;
    // Read before closing: `request` follows ui.connectAs.
    const hostId = request.hostId;
    ui.connectAs = null;
    void connections.connect(hostId, { user: user.trim() });
  }

  const close = () => (ui.connectAs = null);
</script>

<Dialog onclose={close} labelledby="connect-as-title" describedby="connect-as-desc" width={420}>
  <form {onsubmit} class="form">
    <div class="dialog-header">
      <span class="dialog-icon"><UserRound size={18} /></span>
      <div>
        <h2 class="dialog-title" id="connect-as-title">Connect to {host?.name ?? "host"} as…</h2>
        {#if host}<div class="context mono">{host.hostname}</div>{/if}
      </div>
    </div>
    <div class="dialog-body">
      {#if request.error}
        <div class="retry" role="alert">{request.error}</div>
      {/if}
      <p id="connect-as-desc" class="text">
        {#if request.reason === "required"}
          Patchbay doesn’t know which user to log in as on this device yet. Once you’re in, it remembers the user for next time.
        {:else if request.reason === "failed"}
          Check the user name, or try another one.
        {:else}
          Log in as a different user. If it works, Patchbay uses that user from now on.
        {/if}
      </p>
      <label class="field-label" for="connect-as-user">User</label>
      <input
        id="connect-as-user"
        class="input mono"
        bind:value={user}
        autocomplete="off"
        autocapitalize="off"
        spellcheck="false"
        aria-invalid={user.trim() !== "" && !valid}
        data-autofocus
      />
      {#if suggestions.length}
        <div class="suggestions" role="group" aria-label="Suggested users">
          {#each suggestions as s (s)}
            <button type="button" class="chip mono" class:active={s === user.trim()} onclick={() => (user = s)}>{s}</button>
          {/each}
        </div>
      {/if}
      {#if request.reason === "required"}
        <p class="hint">
          To set it for every Tailscale device, add <code>User</code> under <code>Host *.ts.net</code> in <code>~/.ssh/config</code>.
        </p>
      {/if}
    </div>
    <div class="dialog-footer">
      <span class="spacer"></span>
      <button type="button" class="btn" onclick={close}>Cancel</button>
      <button type="submit" class="btn btn-primary" disabled={!valid}>Connect</button>
    </div>
  </form>
</Dialog>

<style>
  .form {
    display: contents;
  }
  .context {
    margin-top: 3px;
    color: var(--text-muted);
    font-size: 11.5px;
  }
  .text {
    color: var(--text);
  }
  .field-label {
    display: block;
    margin: 12px 0 6px;
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
  }
  .input {
    width: 100%;
  }
  .suggestions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 8px;
  }
  .chip {
    height: 24px;
    padding: 0 9px;
    border: 1px solid var(--border);
    border-radius: var(--radius-full);
    background: var(--bg-elevated);
    color: var(--text);
    font-size: var(--text-sm);
  }
  .chip:hover {
    background: var(--bg-hover);
  }
  .chip.active {
    border-color: var(--accent);
    background: var(--accent-soft);
    color: var(--accent-text);
  }
  .retry {
    margin-bottom: 12px;
    padding: 8px 10px;
    border-radius: var(--radius-sm);
    background: var(--danger-soft);
    color: var(--danger-text);
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
    overflow-wrap: anywhere;
  }
  .hint {
    margin-top: 12px;
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
</style>
