<!--
  Tag editor: Enter / comma adds a chip, Backspace on an empty input removes
  the last one. Existing tags are offered as suggestions (listbox popup).
-->
<script lang="ts">
  import X from "@lucide/svelte/icons/x";

  let {
    values = $bindable([]),
    id,
    placeholder = "Add…",
    suggestions = [],
    describedby,
  }: {
    values?: string[];
    id: string;
    placeholder?: string;
    suggestions?: string[];
    describedby?: string;
  } = $props();

  let draft = $state("");
  let input = $state<HTMLInputElement>();
  let box = $state<HTMLDivElement>();
  let open = $state(false);
  let active = $state(-1);
  let rect = $state<{ left: number; top: number | null; bottom: number | null; width: number }>({
    left: 0,
    top: 0,
    bottom: null,
    width: 0,
  });
  const listId = $derived(`${id}-suggestions`);
  const available = $derived(
    suggestions
      .filter((s) => !values.includes(s))
      .filter((s) => !draft.trim() || s.toLowerCase().includes(draft.trim().toLowerCase()))
      .slice(0, 8),
  );
  const showList = $derived(open && available.length > 0);

  function place() {
    if (!box) return;
    const r = box.getBoundingClientRect();
    const below = window.innerHeight - r.bottom;
    // Open upwards when there isn't room below.
    rect =
      below < 180 && r.top > below
        ? { left: r.left, top: null, bottom: window.innerHeight - r.top + 4, width: r.width }
        : { left: r.left, top: r.bottom + 4, bottom: null, width: r.width };
  }

  function commit(raw = draft) {
    const parts = raw
      .split(",")
      .map((s) => s.trim().replace(/\s+/g, "-"))
      .filter(Boolean);
    if (parts.length) values = [...new Set([...values, ...parts])];
    draft = "";
    active = -1;
    open = false;
    place();
  }

  function remove(tag: string) {
    values = values.filter((v) => v !== tag);
    input?.focus();
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      if (!open) {
        place();
        open = true;
      } else active = Math.min(available.length - 1, active + 1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      active = Math.max(-1, active - 1);
    } else if (e.key === "Enter" || e.key === ",") {
      if (e.key === "Enter" && showList && active >= 0) {
        e.preventDefault();
        commit(available[active]);
      } else if (draft.trim()) {
        e.preventDefault();
        commit();
      } else if (e.key === ",") {
        e.preventDefault();
      }
    } else if (e.key === "Backspace" && !draft && values.length) {
      values = values.slice(0, -1);
      place();
    } else if (e.key === "Escape" && showList) {
      e.preventDefault();
      e.stopPropagation();
      open = false;
    }
  }
</script>

<svelte:window onresize={() => (open = false)} />

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="chips" bind:this={box} onclick={() => input?.focus()}>
  {#each values as tag (tag)}
    <span class="chip">
      {tag}
      <button type="button" class="remove" aria-label="Remove tag {tag}" onclick={() => remove(tag)}>
        <X size={12} />
      </button>
    </span>
  {/each}
  <input
    bind:this={input}
    bind:value={draft}
    {id}
    type="text"
    role="combobox"
    autocomplete="off"
    spellcheck="false"
    placeholder={values.length ? "" : placeholder}
    aria-autocomplete="list"
    aria-expanded={showList}
    aria-controls={listId}
    aria-activedescendant={showList && active >= 0 ? `${id}-sugg-${active}` : undefined}
    aria-describedby={describedby}
    {onkeydown}
    oninput={() => {
      place();
      open = true;
      active = -1;
    }}
    onblur={() => {
      open = false;
      commit();
    }}
  />
</div>
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
  {#each available as s, i (s)}
    <li
      id="{id}-sugg-{i}"
      role="option"
      aria-selected={i === active}
      class:active={i === active}
      onpointerdown={(e) => {
        e.preventDefault();
        commit(s);
      }}
      onpointermove={() => (active = i)}
    >
      {s}
    </li>
  {/each}
</ul>

<style>
  .chips {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px;
    min-height: var(--control-h);
    padding: 3px 4px;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    background: var(--bg-input);
    cursor: text;
    transition:
      border-color var(--dur-fast),
      box-shadow var(--dur-fast);
  }
  .chips:hover {
    border-color: var(--text-subtle);
  }
  .chips:focus-within {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    height: 22px;
    padding: 0 2px 0 8px;
    border-radius: var(--radius-xs);
    background: var(--bg-active);
    font-size: var(--text-sm);
    cursor: default;
  }
  .remove {
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    padding: 0;
    border: 0;
    border-radius: 3px;
    background: transparent;
    color: var(--text-muted);
  }
  .remove:hover {
    background: var(--bg-active);
    color: var(--text);
  }
  input {
    flex: 1;
    min-width: 80px;
    height: 22px;
    padding: 0 4px;
    border: 0;
    background: transparent;
    font-size: var(--text-base);
  }
  input::placeholder {
    color: var(--text-subtle);
  }
  input:focus-visible {
    outline: none;
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
    height: 28px;
    padding: 0 8px;
    border-radius: var(--radius-sm);
  }
  li.active {
    background: var(--bg-active);
  }
</style>
