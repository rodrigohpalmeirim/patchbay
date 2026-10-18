<!--
  Offers (once) to add `Include patchbay.conf` to ~/.ssh/config, so ssh and
  other tools see the hosts saved in Patchbay.
-->
<script lang="ts">
  import SquareTerminal from "@lucide/svelte/icons/square-terminal";
  import Dialog from "../components/Dialog.svelte";
  import { hosts } from "../stores/hosts.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { ui } from "../stores/ui.svelte";
  import { tildify } from "../util/format";

  let saving = $state(false);
  const example = $derived(hosts.list.find((h) => h.source === "app")?.name ?? "my-server");

  async function share() {
    saving = true;
    try {
      if (await hosts.setSharing(true)) {
        toasts.success("Added to ~/.ssh/config", `Try ssh ${example} in a terminal.`);
        ui.shareOffer = false;
      }
    } finally {
      saving = false;
    }
  }
</script>

<Dialog onclose={() => (ui.shareOffer = false)} labelledby="share-title" describedby="share-desc" width={480}>
  <div class="dialog-header">
    <span class="dialog-icon"><SquareTerminal size={18} /></span>
    <div>
      <h2 class="dialog-title" id="share-title">Use your Patchbay hosts with ssh too?</h2>
    </div>
  </div>
  <div class="dialog-body">
    <p id="share-desc" class="text">
      Patchbay saves the hosts you add in <code>{tildify(hosts.sharing?.path ?? "~/.ssh/patchbay.conf")}</code>. Adding one line
      to the top of <code>~/.ssh/config</code> lets ssh, scp, VS Code and other tools use them as well, for example
      <code>ssh {example}</code>.
    </p>
    <pre class="snippet"><span class="comment"># Added by Patchbay: lets ssh use the hosts saved in Patchbay.</span>
Include patchbay.conf</pre>
    <p class="hint">You can change this later in Settings.</p>
  </div>
  <div class="dialog-footer">
    <span class="spacer"></span>
    <button type="button" class="btn" onclick={() => (ui.shareOffer = false)}>Not now</button>
    <button type="button" class="btn btn-primary" disabled={saving} onclick={share} data-autofocus>Add the line</button>
  </div>
</Dialog>

<style>
  .text {
    color: var(--text);
  }
  .snippet {
    margin: 12px 0 0;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--bg-inset);
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1.6;
    white-space: pre-wrap;
  }
  .comment {
    color: var(--text-muted);
  }
  .hint {
    margin-top: 10px;
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
</style>
