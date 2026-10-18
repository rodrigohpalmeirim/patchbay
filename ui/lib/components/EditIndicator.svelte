<!-- Sidebar footer button shown while files are being edited in local apps; opens the Editing tab. -->
<script lang="ts">
  import FilePen from "@lucide/svelte/icons/file-pen";
  import LoaderCircle from "@lucide/svelte/icons/loader-circle";
  import { edits } from "../stores/edits.svelte";
  import { transfers } from "../stores/transfers.svelte";
  import { plural } from "../util/format";

  const count = $derived(edits.list.length);
  const attention = $derived(edits.attention.length);
  const busy = $derived(edits.list.some((s) => s.state === "uploading" || s.state === "syncing"));
  const on = $derived(transfers.open && transfers.tab === "editing");
  const label = $derived(
    `Editing ${plural(count, "file")}${attention ? ` (${attention} need${attention === 1 ? "s" : ""} attention)` : ""}`,
  );
</script>

{#if count > 0}
  <button class="ind" class:on class:warn={attention > 0} aria-label={label} aria-pressed={on} title={label} onclick={() => edits.show()}>
    {#if busy}<LoaderCircle size={14} class="spin" />{:else}<FilePen size={14} />{/if}
    <span class="count">{count}</span>
  </button>
{/if}

<style>
  .ind {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 26px;
    padding: 0 7px;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text-muted);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
    transition:
      background var(--dur-fast),
      color var(--dur-fast);
  }
  .ind:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  .ind.on {
    background: var(--bg-active);
    color: var(--text);
  }
  .count {
    font-weight: var(--weight-medium);
  }
  .ind.warn {
    color: var(--warning-text);
  }
</style>
