<script lang="ts">
  import type { Status } from "../stores/connections.svelte";

  let { status, size = 8, ring = false }: { status: Status; size?: number; ring?: boolean } = $props();
</script>

<span class="dot {status}" class:ring style:--size="{size}px" aria-hidden="true"></span>

<style>
  .dot {
    position: relative;
    display: inline-block;
    flex: none;
    width: var(--size);
    height: var(--size);
    border-radius: 50%;
    background: var(--status-off);
  }
  .dot.off {
    background: transparent;
    box-shadow: inset 0 0 0 1.5px var(--status-off);
  }
  .dot.on {
    background: var(--success);
  }
  .dot.error {
    background: var(--danger);
  }
  .dot.busy {
    background: var(--warning);
  }
  .dot.busy::after {
    content: "";
    position: absolute;
    inset: -3px;
    border-radius: 50%;
    border: 1.5px solid var(--warning);
    opacity: 0;
    animation: pulse 1.4s var(--ease-out) infinite;
  }
  .ring {
    outline: 2px solid var(--ring-color, var(--bg));
  }
  @keyframes pulse {
    0% {
      transform: scale(0.6);
      opacity: 0.8;
    }
    100% {
      transform: scale(1.4);
      opacity: 0;
    }
  }
</style>
