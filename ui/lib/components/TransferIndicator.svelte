<!-- Sidebar footer button: transfer activity at a glance; toggles the transfers panel. -->
<script lang="ts">
  import ArrowDownUp from "@lucide/svelte/icons/arrow-down-up";
  import { transfers } from "../stores/transfers.svelte";
  import { modKey } from "../util/platform";

  const active = $derived(transfers.active.length);
  const failed = $derived(transfers.failed.length);
  const waiting = $derived(transfers.active.some((t) => t.state === "waitingForUser"));
  const pct = $derived(transfers.progress);
  const on = $derived(transfers.open && transfers.tab === "transfers");
  const R = 6.5;
  const C = 2 * Math.PI * R;

  const label = $derived.by(() => {
    const parts = [];
    if (active) parts.push(`${active} active`);
    if (failed) parts.push(`${failed} failed`);
    return `Transfers${parts.length ? `: ${parts.join(", ")}` : ""}`;
  });
</script>

<button
  class="ind"
  class:on
  class:busy={active > 0}
  class:failed={failed > 0 && active === 0}
  class:waiting
  aria-label={label}
  aria-pressed={on}
  title="{label} ({modKey}+Shift+J)"
  onclick={() => transfers.toggle()}
>
  {#if active > 0}
    <svg class="ring" width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
      <circle cx="8" cy="8" r={R} class="track" />
      {#if pct == null}
        <circle cx="8" cy="8" r={R} class="arc spinning" stroke-dasharray="{C * 0.28} {C}" />
      {:else}
        <circle cx="8" cy="8" r={R} class="arc" stroke-dasharray="{Math.max(0.5, C * pct)} {C}" />
      {/if}
    </svg>
    <span class="txt">{pct == null ? "" : `${Math.floor(pct * 100)}%`}</span>
    {#if active > 1}<span class="count">{active}</span>{/if}
  {:else}
    <ArrowDownUp size={14} />
    {#if failed > 0}<span class="count danger">{failed}</span>{/if}
  {/if}
</button>

<style>
  .ind {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 26px;
    padding: 0 8px;
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
  .ind.busy {
    color: var(--accent-text);
  }
  .ind.waiting {
    color: var(--warning-text);
  }
  .ring {
    transform: rotate(-90deg);
    flex: none;
  }
  .track {
    fill: none;
    stroke: var(--bg-active);
    stroke-width: 2;
  }
  .arc {
    fill: none;
    stroke: currentColor;
    stroke-width: 2;
    stroke-linecap: round;
    transition: stroke-dasharray 250ms linear;
  }
  .arc.spinning {
    transform-origin: 8px 8px;
    animation: spin 0.9s linear infinite;
  }
  .txt {
    font-weight: var(--weight-medium);
  }
  .count {
    display: inline-grid;
    place-items: center;
    min-width: 16px;
    height: 16px;
    padding: 0 4px;
    border-radius: var(--radius-full);
    background: var(--accent-soft);
    color: var(--accent-text);
    font-size: 10.5px;
    font-weight: var(--weight-semibold);
  }
  .count.danger {
    background: var(--danger-soft);
    color: var(--danger-text);
  }
</style>
