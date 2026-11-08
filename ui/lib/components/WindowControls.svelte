<!--
  Minimize / maximize / close for windows without a system title bar (see
  `customTitlebar`). Pinned to the window's top-right corner, over the end of
  the top-right tab strip, which leaves room for it (`--window-controls-w`).
-->
<script lang="ts">
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { onMount } from "svelte";

  const win = getCurrentWindow();
  let maximized = $state(false);
  let focused = $state(true);

  onMount(() => {
    const sync = async () => {
      maximized = await win.isMaximized();
    };
    void sync();
    const unlisten = [win.onResized(() => void sync()), win.onFocusChanged(({ payload }) => (focused = payload))];
    return () => unlisten.forEach((p) => void p.then((fn) => fn()));
  });
</script>

<div class="controls" class:blurred={!focused} role="group" aria-label="Window">
  <button type="button" tabindex="-1" aria-label="Minimize" title="Minimize" onclick={() => void win.minimize()}>
    <svg viewBox="0 0 10 10" aria-hidden="true"><path d="M0 5.5h10" /></svg>
  </button>
  <button
    type="button"
    tabindex="-1"
    aria-label={maximized ? "Restore" : "Maximize"}
    title={maximized ? "Restore" : "Maximize"}
    onclick={() => void win.toggleMaximize()}
  >
    {#if maximized}
      <svg viewBox="0 0 10 10" aria-hidden="true">
        <path d="M2.5 2.5V1.5a1 1 0 0 1 1-1h5a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1h-1" />
        <rect x="0.5" y="2.5" width="7" height="7" rx="1" />
      </svg>
    {:else}
      <svg viewBox="0 0 10 10" aria-hidden="true"><rect x="0.5" y="0.5" width="9" height="9" rx="1" /></svg>
    {/if}
  </button>
  <button type="button" class="close" tabindex="-1" aria-label="Close" title="Close" onclick={() => void win.close()}>
    <svg viewBox="0 0 10 10" aria-hidden="true"><path d="M0.5 0.5l9 9M9.5 0.5l-9 9" /></svg>
  </button>
</div>

<style>
  .controls {
    position: fixed;
    top: 0;
    right: 0;
    z-index: 50;
    display: flex;
    width: var(--window-controls-w);
    height: var(--tabstrip-h);
  }
  button {
    display: flex;
    flex: 1;
    align-items: center;
    justify-content: center;
    padding: 0;
    border: 0;
    border-radius: 0;
    background: transparent;
    color: var(--text);
    transition: background var(--dur-fast);
  }
  .blurred button {
    color: var(--text-subtle);
  }
  button:hover {
    background: var(--bg-active);
    color: var(--text);
  }
  button:active {
    background: var(--bg-hover);
  }
  .close:hover {
    background: #c42b1c;
    color: #fff;
  }
  .close:active {
    background: #c84031;
    color: rgba(255, 255, 255, 0.8);
  }
  svg {
    width: 10px;
    height: 10px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1;
    shape-rendering: geometricPrecision;
  }
</style>
