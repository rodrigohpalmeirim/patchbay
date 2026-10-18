<script lang="ts">
  import { fly } from "svelte/transition";
  import CircleAlert from "@lucide/svelte/icons/circle-alert";
  import CircleCheck from "@lucide/svelte/icons/circle-check";
  import Info from "@lucide/svelte/icons/info";
  import X from "@lucide/svelte/icons/x";
  import { toasts } from "../stores/toasts.svelte";

  const reduced =
    typeof window !== "undefined" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
</script>

<div class="toasts" aria-live="polite" aria-relevant="additions">
  {#each toasts.items as t (t.id)}
    <div
      class="toast {t.kind}"
      role={t.kind === "error" ? "alert" : "status"}
      transition:fly={{ y: 12, duration: reduced ? 0 : 180 }}
      onpointerenter={() => toasts.hold(t.id)}
      onpointerleave={() => toasts.release(t.id)}
      onfocusin={() => toasts.hold(t.id)}
      onfocusout={() => toasts.release(t.id)}
    >
      <span class="icon">
        {#if t.kind === "error"}
          <CircleAlert size={16} />
        {:else if t.kind === "success"}
          <CircleCheck size={16} />
        {:else}
          <Info size={16} />
        {/if}
      </span>
      <div class="body">
        <div class="title">{t.title}</div>
        {#if t.message}<div class="message selectable">{t.message}</div>{/if}
        {#if t.action}
          {@const action = t.action}
          <button
            class="btn btn-primary btn-sm action"
            onclick={() => {
              toasts.dismiss(t.id);
              action.run();
            }}>{action.label}</button
          >
        {/if}
      </div>
      <button class="icon-btn sm" aria-label="Dismiss notification" onclick={() => toasts.dismiss(t.id)}>
        <X size={14} />
      </button>
    </div>
  {/each}
</div>

<style>
  .toasts {
    position: fixed;
    right: 16px;
    bottom: 16px;
    z-index: 900;
    display: flex;
    flex-direction: column;
    gap: 8px;
    width: min(360px, calc(100vw - 32px));
    pointer-events: none;
  }
  .toast {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 10px 8px 10px 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    box-shadow: var(--shadow-lg);
    pointer-events: auto;
  }
  .icon {
    display: flex;
    margin-top: 1px;
    color: var(--accent-text);
  }
  .error .icon {
    color: var(--danger-text);
  }
  .success .icon {
    color: var(--success-text);
  }
  .body {
    flex: 1;
    min-width: 0;
  }
  .title {
    font-weight: var(--weight-medium);
  }
  .action {
    margin-top: 8px;
  }
  .message {
    margin-top: 2px;
    color: var(--text-muted);
    font-size: var(--text-sm);
    overflow-wrap: anywhere;
  }
</style>
