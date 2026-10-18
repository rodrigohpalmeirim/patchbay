<!--
  Modal dialog built on the native <dialog> element: it renders in the top
  layer, makes the rest of the page inert and traps focus for free. Mount it
  to open, unmount to close; focus returns to the previously focused element.
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import { onMount } from "svelte";

  let {
    onclose,
    labelledby,
    describedby,
    width = 440,
    tone = "default",
    dismissable = true,
    role = "dialog",
    children,
  }: {
    /** Called on Esc / backdrop click / native close. */
    onclose: () => void;
    labelledby?: string;
    describedby?: string;
    width?: number;
    tone?: "default" | "danger";
    /** Close when clicking the backdrop. */
    dismissable?: boolean;
    role?: "dialog" | "alertdialog";
    children: Snippet;
  } = $props();

  let el: HTMLDialogElement;
  let alive = true;

  onMount(() => {
    const previous = document.activeElement as HTMLElement | null;
    el.showModal();
    const target = el.querySelector<HTMLElement>("[data-autofocus]");
    target?.focus();
    if (target instanceof HTMLInputElement) target.select();
    return () => {
      alive = false;
      if (previous && previous.isConnected) previous.focus();
    };
  });

  function oncancel(e: Event) {
    e.preventDefault();
    if (alive) onclose();
  }

  // Fallback: the browser may force-close (e.g. repeated Esc).
  function onnativeclose() {
    if (alive) onclose();
  }

  // Click on the ::backdrop is dispatched to the <dialog> itself.
  function onclick(e: MouseEvent) {
    if (dismissable && alive && e.target === el) onclose();
  }
</script>

<dialog
  bind:this={el}
  class="dialog"
  class:danger={tone === "danger"}
  style:--dialog-w="{width}px"
  aria-labelledby={labelledby}
  aria-describedby={describedby}
  role={role === "alertdialog" ? "alertdialog" : undefined}
  {oncancel}
  {onclick}
  onclose={onnativeclose}
>
  <div class="panel">
    {@render children()}
  </div>
</dialog>

<style>
  .dialog {
    width: min(var(--dialog-w), calc(100vw - 32px));
    max-width: none;
    max-height: calc(100vh - 32px);
    padding: 0;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-elevated);
    color: var(--text);
    box-shadow: var(--shadow-lg);
    overflow: hidden;
    animation: dialog-in var(--dur) var(--ease-out);
  }
  .dialog.danger {
    border-color: var(--danger-border);
  }
  .dialog::backdrop {
    background: var(--bg-backdrop);
    animation: fade-in var(--dur) var(--ease-out);
  }
  .panel {
    display: flex;
    flex-direction: column;
    max-height: calc(100vh - 34px);
  }
  @keyframes dialog-in {
    from {
      opacity: 0;
      transform: translateY(6px) scale(0.98);
    }
  }
  @keyframes fade-in {
    from {
      opacity: 0;
    }
  }

  /* Shared dialog layout pieces, used by every dialog via :global. */
  .dialog :global(.dialog-header) {
    display: flex;
    align-items: flex-start;
    gap: var(--space-3);
    padding: var(--space-5) var(--space-5) var(--space-3);
  }
  .dialog :global(.dialog-title) {
    font-size: var(--text-md);
    font-weight: var(--weight-semibold);
    line-height: 1.35;
  }
  .dialog :global(.dialog-subtitle) {
    margin-top: 2px;
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .dialog :global(.dialog-body) {
    padding: 0 var(--space-5) var(--space-4);
    overflow-y: auto;
    min-height: 0;
  }
  .dialog :global(.dialog-footer) {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-5);
    border-top: 1px solid var(--border-subtle);
    background: var(--bg-subtle);
  }
  .dialog :global(.dialog-footer .spacer) {
    flex: 1;
  }
  .dialog :global(.dialog-icon) {
    display: grid;
    place-items: center;
    flex: none;
    width: 36px;
    height: 36px;
    border-radius: var(--radius-md);
    background: var(--accent-soft);
    color: var(--accent-text);
  }
  .dialog :global(.dialog-icon.danger) {
    background: var(--danger-soft);
    color: var(--danger-text);
  }
  .dialog :global(.dialog-icon.warning) {
    background: var(--warning-soft);
    color: var(--warning-text);
  }
</style>
