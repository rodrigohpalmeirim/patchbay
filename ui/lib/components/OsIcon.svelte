<script lang="ts">
  import type { RemoteOs } from "../types";
  import Apple from "@lucide/svelte/icons/apple";
  import CircleHelp from "@lucide/svelte/icons/circle-help";
  import Server from "@lucide/svelte/icons/server";

  let { os, size = 18 }: { os: RemoteOs; size?: number } = $props();
</script>

{#if os === "windows"}
  <svg width={size} height={size} viewBox="0 0 24 24" aria-hidden="true">
    <g fill="currentColor">
      <rect x="3" y="3" width="8.4" height="8.4" rx="1" />
      <rect x="12.6" y="3" width="8.4" height="8.4" rx="1" />
      <rect x="3" y="12.6" width="8.4" height="8.4" rx="1" />
      <rect x="12.6" y="12.6" width="8.4" height="8.4" rx="1" />
    </g>
  </svg>
{:else if os === "linux"}
  <!-- Simplified penguin silhouette. -->
  <svg width={size} height={size} viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor"
    stroke-width="1.8" stroke-linejoin="round" stroke-linecap="round">
    <path d="M12 2.5c-2.6 0-4 2.1-4 4.6 0 1.6-.6 2.7-1.6 4.2C5.2 13 4.5 14.8 4.8 17c.2 1.4 1.3 2.3 2.4 2.6" />
    <path d="M12 2.5c2.6 0 4 2.1 4 4.6 0 1.6.6 2.7 1.6 4.2 1.2 1.7 1.9 3.5 1.6 5.7-.2 1.4-1.3 2.3-2.4 2.6" />
    <path d="M7.2 19.6c1.2.9 3 1.4 4.8 1.4s3.6-.5 4.8-1.4" />
    <path d="M9.6 9.2c.6.8 1.4 1.2 2.4 1.2s1.8-.4 2.4-1.2" />
    <circle cx="10.4" cy="6.6" r=".4" fill="currentColor" />
    <circle cx="13.6" cy="6.6" r=".4" fill="currentColor" />
  </svg>
{:else if os === "macOs"}
  <Apple {size} />
{:else if os === "unknown"}
  <CircleHelp {size} />
{:else}
  <Server {size} />
{/if}
