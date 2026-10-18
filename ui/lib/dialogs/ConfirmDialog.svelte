<script lang="ts">
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";
  import Dialog from "../components/Dialog.svelte";
  import type { ConfirmState } from "../stores/ui.svelte";

  let { state: cs }: { state: ConfirmState } = $props();
  const MAX = 8;
  let checked = $state(false);
</script>

<Dialog
  onclose={() => cs.resolve(false)}
  labelledby="confirm-title"
  describedby="confirm-desc"
  width={cs.details?.length ? 440 : 400}
  role="alertdialog"
>
  <form
    class="form"
    onsubmit={(e) => {
      e.preventDefault();
      cs.resolve(true, checked);
    }}
  >
    <div class="dialog-header">
      {#if cs.danger}
        <span class="dialog-icon danger"><TriangleAlert size={18} /></span>
      {/if}
      <div>
        <h2 class="dialog-title" id="confirm-title">{cs.title}</h2>
        <p class="dialog-subtitle message" id="confirm-desc">{cs.message}</p>
      </div>
    </div>
    {#if cs.details?.length}
      <div class="dialog-body">
        <ul class="details" aria-label="Affected items">
          {#each cs.details.slice(0, MAX) as item, i (i)}
            <li class="mono selectable" title={item}>{item}</li>
          {/each}
          {#if cs.details.length > MAX}
            <li class="more">and {(cs.details.length - MAX).toLocaleString()} more</li>
          {/if}
        </ul>
      </div>
    {/if}
    <div class="dialog-footer">
      {#if cs.option}
        <label class="checkbox option"><input type="checkbox" bind:checked /> {cs.option}</label>
        <span class="spacer"></span>
      {/if}
      <button type="button" class="btn" onclick={() => cs.resolve(false, checked)} data-autofocus>Cancel</button>
      <button type="submit" class="btn" class:btn-danger={cs.danger} class:btn-primary={!cs.danger}>
        {cs.confirmLabel}
      </button>
    </div>
  </form>
</Dialog>

<style>
  .form {
    display: contents;
  }
  .message {
    margin-top: 4px;
    font-size: var(--text-base);
  }
  .details {
    margin: 0;
    padding: 6px 0;
    list-style: none;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--bg-inset);
    max-height: 220px;
    overflow-y: auto;
  }
  .details li {
    padding: 3px 12px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: 12px;
  }
  .option {
    font-size: var(--text-sm);
  }
  .details .more {
    color: var(--text-muted);
    font-family: var(--font-sans);
  }
</style>
