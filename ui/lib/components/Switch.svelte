<!-- On/off switch (ARIA switch). `busy` shows a spinner in the thumb while a change is in flight. -->
<script lang="ts">
  let {
    checked,
    onchange,
    label,
    busy = false,
    disabled = false,
    tone = "accent",
    title,
  }: {
    checked: boolean;
    onchange: (checked: boolean) => void;
    /** Accessible name. */
    label: string;
    busy?: boolean;
    disabled?: boolean;
    /** Track color when on. */
    tone?: "accent" | "warning" | "danger";
    title?: string;
  } = $props();
</script>

<button
  type="button"
  role="switch"
  class="switch {tone}"
  class:on={checked}
  class:busy
  aria-checked={checked}
  aria-label={label}
  aria-busy={busy || undefined}
  {title}
  {disabled}
  onclick={() => onchange(!checked)}
>
  <span class="thumb" aria-hidden="true"></span>
</button>

<style>
  .switch {
    position: relative;
    flex: none;
    width: 32px;
    height: 18px;
    padding: 0;
    border: 0;
    border-radius: var(--radius-full);
    background: var(--border-strong);
    transition: background var(--dur) var(--ease-out);
  }
  .switch:hover:not(:disabled) {
    background: var(--text-subtle);
  }
  .switch.on {
    background: var(--accent);
  }
  .switch.on:hover:not(:disabled) {
    background: var(--accent-hover);
  }
  .switch.on.warning {
    background: var(--warning);
  }
  .switch.on.danger {
    background: var(--danger);
  }
  .switch:focus-visible {
    outline-offset: 2px;
  }
  .switch:disabled {
    opacity: 0.5;
  }
  .thumb {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.25);
    transition: transform var(--dur) var(--ease-out);
  }
  .on .thumb {
    transform: translateX(14px);
  }
  /* In-flight: a small arc spins inside the thumb. */
  .busy .thumb::after {
    content: "";
    position: absolute;
    inset: 3px;
    border: 1.5px solid var(--accent);
    border-right-color: transparent;
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }
  .busy.warning .thumb::after {
    border-color: var(--warning);
    border-right-color: transparent;
  }
</style>
