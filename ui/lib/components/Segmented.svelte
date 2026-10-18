<!--
  Segmented control built on native radio inputs (arrow keys move the
  selection, one tab stop). Options may carry an icon.
-->
<script lang="ts" generics="T extends string">
  import type { LucideIcon } from "@lucide/svelte";

  let {
    value = $bindable(),
    options,
    name,
    label,
    labelledby,
    size = "md",
    disabled = false,
    onchange,
  }: {
    value: T;
    options: { value: T; label: string; icon?: LucideIcon; title?: string }[];
    /** Radio group name (unique per form). */
    name: string;
    label?: string;
    labelledby?: string;
    size?: "sm" | "md";
    disabled?: boolean;
    onchange?: (value: T) => void;
  } = $props();
</script>

<div
  class="segmented {size}"
  class:disabled
  role="radiogroup"
  aria-label={label}
  aria-labelledby={labelledby}
  style:--cols={options.length}
>
  {#each options as o (o.value)}
    <label class:checked={value === o.value} title={o.title}>
      <input
        type="radio"
        {name}
        value={o.value}
        checked={value === o.value}
        {disabled}
        onchange={() => {
          value = o.value;
          onchange?.(o.value);
        }}
      />
      {#if o.icon}<o.icon size={14} />{/if}
      <span>{o.label}</span>
    </label>
  {/each}
</div>

<style>
  .segmented {
    display: grid;
    grid-template-columns: repeat(var(--cols), minmax(0, 1fr));
    gap: 2px;
    padding: 3px;
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    background: var(--bg-inset);
  }
  label {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    min-width: 0;
    height: 28px;
    padding: 0 8px;
    border-radius: var(--radius-sm);
    color: var(--text-muted);
    font-weight: var(--weight-medium);
    white-space: nowrap;
    transition:
      background var(--dur-fast),
      color var(--dur-fast);
  }
  label span {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .sm label {
    height: 24px;
    font-size: var(--text-sm);
  }
  label:hover:not(.checked) {
    color: var(--text);
  }
  label:has(input:focus-visible) {
    outline: 2px solid var(--focus-ring);
    outline-offset: 1px;
  }
  input {
    position: absolute;
    opacity: 0;
    pointer-events: none;
  }
  label.checked {
    background: var(--bg-elevated);
    color: var(--text);
    box-shadow: var(--shadow-sm), 0 0 0 1px var(--border-subtle);
  }
  .disabled {
    opacity: 0.55;
  }
  .disabled label:hover:not(.checked) {
    color: var(--text-muted);
  }
</style>
