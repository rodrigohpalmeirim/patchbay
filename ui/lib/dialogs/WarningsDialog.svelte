<script lang="ts">
  import RefreshCw from "@lucide/svelte/icons/refresh-cw";
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";
  import Dialog from "../components/Dialog.svelte";
  import { hosts } from "../stores/hosts.svelte";
  import { ui } from "../stores/ui.svelte";

  const close = () => (ui.warningsOpen = false);
</script>

<Dialog onclose={close} labelledby="warnings-title" describedby="warnings-desc" width={560}>
  <div class="dialog-header">
    <span class="dialog-icon warning"><TriangleAlert size={18} /></span>
    <div>
      <h2 class="dialog-title" id="warnings-title">SSH config warnings</h2>
      <p class="dialog-subtitle" id="warnings-desc">
        These lines were skipped or only partly understood. The rest of your config still works.
      </p>
    </div>
  </div>
  <div class="dialog-body">
    {#if hosts.warnings.length}
      <ul class="list">
        {#each hosts.warnings as w, i (i)}
          <li class="mono selectable">{w}</li>
        {/each}
      </ul>
    {:else}
      <p class="none">No warnings — everything parsed cleanly.</p>
    {/if}
  </div>
  <div class="dialog-footer">
    <button class="btn" onclick={() => hosts.reload()} disabled={hosts.reloading}>
      <RefreshCw size={13} class={hosts.reloading ? "spin" : ""} /> Reload config
    </button>
    <span class="spacer"></span>
    <button class="btn btn-primary" onclick={close} data-autofocus>Done</button>
  </div>
</Dialog>

<style>
  .list {
    margin: 0;
    padding: 0;
    list-style: none;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--bg-inset);
    overflow: hidden;
  }
  li {
    padding: 9px 12px;
    font-size: 12px;
    line-height: 1.5;
    overflow-wrap: anywhere;
  }
  li + li {
    border-top: 1px solid var(--border);
  }
  .none {
    color: var(--text-muted);
  }
</style>
