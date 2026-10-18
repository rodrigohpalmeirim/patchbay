<!--
  Text input with a filtered suggestion list (ARIA 1.2 combobox pattern).
  `multi` treats the value as a comma-separated list and completes the last
  item (used for ProxyJump chains).
-->
<script lang="ts">
  import { tick } from "svelte";
  import type { HTMLInputAttributes } from "svelte/elements";

  let {
    value = $bindable(""),
    options,
    multi = false,
    id,
    invalid = false,
    describedby,
    placeholder,
    mono = false,
    describe,
    onblur: userBlur,
    ...rest
  }: {
    value?: string;
    options: string[];
    multi?: boolean;
    id: string;
    invalid?: boolean;
    describedby?: string;
    placeholder?: string;
    mono?: boolean;
    /** Optional secondary text per option. */
    describe?: (option: string) => string | undefined;
  } & Omit<HTMLInputAttributes, "value" | "id"> = $props();

  let input = $state<HTMLInputElement>();
  let open = $state(false);
  let active = $state(-1);
  let rect = $state<{ left: number; top: number | null; bottom: number | null; width: number }>({
    left: 0,
    top: 0,
    bottom: null,
    width: 0,
  });
  const listId = $derived(`${id}-listbox`);

  const term = $derived((multi ? (value.split(",").at(-1) ?? "") : value).trim().toLowerCase());
  const taken = $derived(
    multi
      ? new Set(
          value
            .split(",")
            .slice(0, -1)
            .map((s) => s.trim().toLowerCase()),
        )
      : new Set<string>(),
  );
  const filtered = $derived(
    options
      .filter((o) => !taken.has(o.toLowerCase()))
      .filter((o) => !term || o.toLowerCase().includes(term))
      .filter((o) => o.toLowerCase() !== term)
      .slice(0, 8),
  );
  const showList = $derived(open && filtered.length > 0);

  function place() {
    if (!input) return;
    const r = input.getBoundingClientRect();
    const below = window.innerHeight - r.bottom;
    // Open upwards when there isn't room below.
    rect =
      below < 180 && r.top > below
        ? { left: r.left, top: null, bottom: window.innerHeight - r.top + 4, width: r.width }
        : { left: r.left, top: r.bottom + 4, bottom: null, width: r.width };
  }

  function show() {
    place();
    open = true;
    active = -1;
  }

  function choose(option: string) {
    if (multi) {
      const parts = value.split(",").map((s) => s.trim());
      parts[parts.length - 1] = option;
      value = parts.filter((p, i) => p || i === parts.length - 1).join(", ");
    } else {
      value = option;
    }
    open = false;
    void tick().then(() => input?.focus());
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      if (!open) show();
      else active = Math.min(filtered.length - 1, active + 1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      active = Math.max(-1, active - 1);
    } else if (e.key === "Enter" && showList && active >= 0) {
      e.preventDefault();
      choose(filtered[active]);
    } else if (e.key === "Escape" && showList) {
      // Close the list without closing the surrounding dialog.
      e.preventDefault();
      e.stopPropagation();
      open = false;
    } else if (e.key === "Tab") {
      open = false;
    }
  }
</script>

<svelte:window onresize={() => (open = false)} />

<div class="combo">
  <input
    bind:this={input}
    bind:value
    {id}
    {placeholder}
    class="input"
    class:mono
    type="text"
    role="combobox"
    autocomplete="off"
    spellcheck="false"
    aria-autocomplete="list"
    aria-expanded={showList}
    aria-controls={listId}
    aria-activedescendant={showList && active >= 0 ? `${id}-opt-${active}` : undefined}
    aria-invalid={invalid}
    aria-describedby={describedby}
    oninput={() => {
      if (!open) show();
      active = -1;
    }}
    onblur={(e) => {
      open = false;
      userBlur?.(e);
    }}
    onclick={() => !open && show()}
    {onkeydown}
    {...rest}
  />
  <ul
    id={listId}
    class="list"
    role="listbox"
    hidden={!showList}
    style:left="{rect.left}px"
    style:top={rect.top === null ? null : `${rect.top}px`}
    style:bottom={rect.bottom === null ? null : `${rect.bottom}px`}
    style:width="{rect.width}px"
  >
    {#each filtered as option, i (option)}
      <li
        id="{id}-opt-{i}"
        role="option"
        aria-selected={i === active}
        class:active={i === active}
        onpointerdown={(e) => {
          e.preventDefault();
          choose(option);
        }}
        onpointermove={() => (active = i)}
      >
        <span class:mono>{option}</span>
        {#if describe?.(option)}<span class="desc">{describe(option)}</span>{/if}
      </li>
    {/each}
  </ul>
</div>

<style>
  .combo {
    position: relative;
  }
  .list {
    position: fixed;
    z-index: 50;
    max-height: 220px;
    margin: 0;
    padding: 4px;
    overflow-y: auto;
    list-style: none;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    box-shadow: var(--shadow-md);
  }
  .list[hidden] {
    display: none;
  }
  li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    height: 28px;
    padding: 0 8px;
    border-radius: var(--radius-sm);
    white-space: nowrap;
  }
  li.active {
    background: var(--bg-active);
  }
  li span {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .desc {
    color: var(--text-subtle);
    font-size: var(--text-sm);
  }
</style>
