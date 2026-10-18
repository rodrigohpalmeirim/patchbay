<!--
  Manage snippets: list on the left, editor on the right (stacked on narrow
  windows). Snippets are global or limited to chosen hosts; "run" presses
  Enter after inserting.
-->
<script lang="ts">
  import ArrowLeft from "@lucide/svelte/icons/arrow-left";
  import Play from "@lucide/svelte/icons/play";
  import Plus from "@lucide/svelte/icons/plus";
  import Search from "@lucide/svelte/icons/search";
  import SquareCode from "@lucide/svelte/icons/square-code";
  import Trash2 from "@lucide/svelte/icons/trash-2";
  import X from "@lucide/svelte/icons/x";
  import { onMount, tick } from "svelte";
  import Dialog from "../components/Dialog.svelte";
  import { hosts } from "../stores/hosts.svelte";
  import { snippets } from "../stores/snippets.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { ui, type SnippetsDialogState } from "../stores/ui.svelte";
  import type { HostId, Snippet } from "../types";
  import { address, collator } from "../util/format";
  import { matchField } from "../util/fuzzy";
  import { hasMod, modKey } from "../util/platform";

  let { state: initial }: { state: SnippetsDialogState } = $props();

  interface Form {
    name: string;
    command: string;
    scope: "all" | "hosts";
    hostIds: HostId[];
    run: boolean;
  }

  const blank = (hostId?: HostId | null): Form => ({ name: "", command: "", scope: "all", hostIds: hostId ? [hostId] : [], run: false });
  const fromSnippet = (s: Snippet): Form => ({
    name: s.name,
    command: s.command,
    scope: s.hostIds.length ? "hosts" : "all",
    hostIds: [...s.hostIds],
    run: s.run,
  });

  let selected = $state<string | "new" | null>(null);
  let form = $state<Form>(blank());
  let baseline = $state("");
  let filter = $state("");
  let hostFilter = $state("");
  let saving = $state(false);
  let submitted = $state(false);
  /** Narrow layout: which side is shown. */
  let showEditor = $state(false);
  let width = $state(760);
  let nameInput = $state<HTMLInputElement>();

  const narrow = $derived(width < 600);
  const serialize = (f: Form) => JSON.stringify({ ...f, hostIds: f.scope === "hosts" ? [...f.hostIds].sort() : [] });
  const dirty = $derived(selected !== null && serialize(form) !== baseline);

  function load(id: string | "new" | null) {
    selected = id;
    const s = id && id !== "new" ? snippets.list.find((x) => x.id === id) : null;
    form = s ? fromSnippet(s) : blank(initial.hostId);
    baseline = serialize(form);
    submitted = false;
    hostFilter = "";
  }

  onMount(async () => {
    await snippets.load();
    const want = initial.select;
    if (want === "new") load("new");
    else if (want && snippets.list.some((s) => s.id === want)) load(want);
    else load(snippets.sorted[0]?.id ?? "new");
    showEditor = !!want || snippets.list.length === 0;
    if (want === "new") void tick().then(() => nameInput?.focus());
  });

  async function confirmDiscard(): Promise<boolean> {
    if (!dirty) return true;
    return ui.confirm({
      title: "Discard unsaved changes?",
      message: `Your changes to ${form.name.trim() ? `“${form.name.trim()}”` : "this snippet"} haven't been saved.`,
      confirmLabel: "Discard",
      danger: true,
    });
  }

  async function select(id: string | "new") {
    if (id === selected) {
      showEditor = true;
      return;
    }
    if (!(await confirmDiscard())) return;
    load(id);
    showEditor = true;
    if (id === "new") void tick().then(() => nameInput?.focus());
  }

  async function close() {
    if (!(await confirmDiscard())) return;
    ui.snippets = null;
  }

  const errors = $derived({
    name: form.name.trim() ? null : "Give the snippet a name",
    command: form.command.trim() ? null : "Enter the command to insert",
    hosts: form.scope === "hosts" && form.hostIds.length === 0 ? "Choose at least one host" : null,
  });
  const valid = $derived(!errors.name && !errors.command && !errors.hosts);

  async function save() {
    submitted = true;
    if (!valid || saving || selected === null) return;
    saving = true;
    const draft = {
      name: form.name.trim(),
      command: form.command.replace(/\s+$/, ""),
      hostIds: form.scope === "hosts" ? [...form.hostIds] : [],
      run: form.run,
    };
    try {
      const s = selected === "new" ? await snippets.create(draft) : await snippets.update(selected, draft);
      load(s.id);
      toasts.success(`Saved “${s.name}”`);
    } catch {
      // toasted
    } finally {
      saving = false;
    }
  }

  async function remove() {
    if (!selected || selected === "new") {
      load(snippets.sorted[0]?.id ?? "new");
      if (narrow) showEditor = false;
      return;
    }
    const s = snippets.list.find((x) => x.id === selected);
    if (!s) return;
    const ok = await ui.confirm({
      title: `Delete “${s.name}”?`,
      message: "The snippet is removed from all hosts. This can't be undone.",
      confirmLabel: "Delete",
      danger: true,
    });
    if (!ok) return;
    const i = snippets.sorted.findIndex((x) => x.id === s.id);
    try {
      await snippets.remove(s.id);
    } catch {
      return;
    }
    const next = snippets.sorted[Math.min(i, snippets.sorted.length - 1)];
    load(next?.id ?? "new");
    if (narrow) showEditor = false;
  }

  function onkeydown(e: KeyboardEvent) {
    if (hasMod(e) && (e.key === "Enter" || e.key.toLowerCase() === "s")) {
      e.preventDefault();
      void save();
    }
  }

  const listed = $derived.by(() => {
    const q = filter.trim().toLowerCase();
    if (!q) return snippets.sorted;
    return snippets.sorted.filter((s) => matchField(q, s.name) || s.command.toLowerCase().includes(q));
  });

  function scopeSummary(s: Snippet): string {
    if (!s.hostIds.length) return "All hosts";
    const names = s.hostIds.map((id) => hosts.get(id)?.name).filter(Boolean) as string[];
    if (!names.length) return "No existing hosts";
    return names.length <= 2 ? names.join(", ") : `${names[0]} +${names.length - 1}`;
  }

  const hostOptions = $derived.by(() => {
    const q = hostFilter.trim().toLowerCase();
    const all = [...hosts.list].sort((a, b) => collator.compare(a.name, b.name));
    // Keep ids of hosts that no longer exist visible so they can be removed.
    const missing = form.hostIds.filter((id) => !hosts.byId.has(id));
    const rows = all
      .filter((h) => !q || h.name.toLowerCase().includes(q) || h.hostname.toLowerCase().includes(q))
      .map((h) => ({ id: h.id, name: h.name, detail: address(h) }));
    return [...rows, ...missing.map((id) => ({ id, name: "Deleted host", detail: id }))];
  });

  function toggleHost(id: HostId, on: boolean) {
    form.hostIds = on ? [...new Set([...form.hostIds, id])] : form.hostIds.filter((x) => x !== id);
  }

  const lineCount = $derived(form.command.split("\n").length);
</script>

<Dialog onclose={() => void close()} labelledby="snippets-title" width={780} dismissable={false}>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="wrap" bind:clientWidth={width} {onkeydown}>
    <div class="dialog-header">
      <span class="dialog-icon"><SquareCode size={18} /></span>
      <div class="head">
        <h2 class="dialog-title" id="snippets-title">Snippets</h2>
        <p class="dialog-subtitle">Saved commands you can insert into any terminal with {modKey === "⌘" ? "⌘⇧S" : "Ctrl+Shift+S"}.</p>
      </div>
      <button class="icon-btn close" aria-label="Close snippets" onclick={() => void close()}><X size={15} /></button>
    </div>

    <div class="body" class:narrow>
      {#if !narrow || !showEditor}
        <section class="side" aria-label="Snippet list">
          <div class="side-top">
            <div class="filter">
              <Search size={13} class="f-icon" />
              <input bind:value={filter} placeholder="Filter" aria-label="Filter snippets" spellcheck="false" autocomplete="off" />
            </div>
            <button class="btn btn-sm" onclick={() => void select("new")} title="New snippet"><Plus size={13} /> New</button>
          </div>
          <ul class="items" role="listbox" aria-label="Snippets">
            {#if selected === "new"}
              <li role="option" aria-selected="true" class="item active draft">
                <span class="item-name">{form.name.trim() || "New snippet"}</span>
                <span class="item-sub">Not saved yet</span>
              </li>
            {/if}
            {#each listed as s (s.id)}
              <li role="option" aria-selected={selected === s.id} class="item" class:active={selected === s.id}>
                <button type="button" class="item-btn" onclick={() => void select(s.id)}>
                  <span class="item-top">
                    <span class="item-name">{s.name}</span>
                    {#if s.run}<Play size={10} class="run-icon" aria-label="Runs immediately" />{/if}
                  </span>
                  <span class="item-sub">{scopeSummary(s)}</span>
                </button>
              </li>
            {:else}
              {#if snippets.loaded && selected !== "new"}
                <li class="none">{filter ? "No matches" : "No snippets yet"}</li>
              {/if}
            {/each}
          </ul>
        </section>
      {/if}

      {#if !narrow || showEditor}
        <section class="editor" aria-label="Edit snippet">
          {#if narrow}
            <button class="btn btn-ghost btn-sm back" onclick={() => (showEditor = false)}><ArrowLeft size={13} /> All snippets</button>
          {/if}
          {#if selected !== null}
            <div class="field">
              <label for="sn-name">Name</label>
              <input
                id="sn-name"
                class="input"
                bind:this={nameInput}
                bind:value={form.name}
                placeholder="e.g. Restart app service"
                autocomplete="off"
                aria-invalid={submitted && !!errors.name}
                aria-describedby="sn-name-msg"
              />
              {#if submitted && errors.name}<span class="error" id="sn-name-msg">{errors.name}</span>{/if}
            </div>
            <div class="field">
              <label for="sn-cmd">Command</label>
              <textarea
                id="sn-cmd"
                class="input mono cmd"
                bind:value={form.command}
                rows={Math.min(8, Math.max(3, lineCount))}
                placeholder="sudo systemctl restart app.service"
                spellcheck="false"
                autocomplete="off"
                autocapitalize="off"
                aria-invalid={submitted && !!errors.command}
                aria-describedby="sn-cmd-msg"
              ></textarea>
              {#if submitted && errors.command}
                <span class="error" id="sn-cmd-msg">{errors.command}</span>
              {:else}
                <span class="help" id="sn-cmd-msg">Inserted at the prompt as typed. Several lines are pasted together.</span>
              {/if}
            </div>

            <fieldset class="field">
              <legend>Available on</legend>
              <div class="segmented" role="radiogroup" aria-label="Available on">
                <label class:checked={form.scope === "all"}><input type="radio" bind:group={form.scope} value="all" /> All hosts</label>
                <label class:checked={form.scope === "hosts"}><input type="radio" bind:group={form.scope} value="hosts" /> Selected hosts</label>
              </div>
              {#if form.scope === "hosts"}
                <div class="hosts" class:invalid={submitted && !!errors.hosts}>
                  {#if hosts.list.length > 6}
                    <div class="hosts-filter">
                      <Search size={12} class="f-icon" />
                      <input bind:value={hostFilter} placeholder="Filter hosts" aria-label="Filter hosts" spellcheck="false" autocomplete="off" />
                      <span class="count">{form.hostIds.length} selected</span>
                    </div>
                  {/if}
                  <ul class="host-list" aria-label="Hosts">
                    {#each hostOptions as h (h.id)}
                      <li>
                        <label class="host-row">
                          <input type="checkbox" checked={form.hostIds.includes(h.id)} onchange={(e) => toggleHost(h.id, e.currentTarget.checked)} />
                          <span class="host-name">{h.name}</span>
                          <span class="host-detail mono">{h.detail}</span>
                        </label>
                      </li>
                    {:else}
                      <li class="none">No hosts match</li>
                    {/each}
                  </ul>
                </div>
                {#if submitted && errors.hosts}<span class="error">{errors.hosts}</span>{/if}
              {:else}
                <span class="help">Shown in every terminal, including local ones.</span>
              {/if}
            </fieldset>

            <label class="checkbox run">
              <input type="checkbox" bind:checked={form.run} />
              <span>
                <span class="run-label">Run immediately</span>
                <span class="help">Press Enter after inserting the command.</span>
              </span>
            </label>
          {/if}
        </section>
      {/if}
    </div>

    <div class="dialog-footer">
      {#if selected !== null && (!narrow || showEditor)}
        <button class="btn btn-danger-outline" onclick={() => void remove()} disabled={saving}>
          <Trash2 size={13} />
          {selected === "new" ? "Discard" : "Delete"}
        </button>
      {/if}
      <span class="spacer"></span>
      {#if dirty}<span class="unsaved">Unsaved changes</span>{/if}
      <button class="btn" onclick={() => void close()}>Close</button>
      {#if selected !== null && (!narrow || showEditor)}
        <button class="btn btn-primary" onclick={() => void save()} disabled={saving || (!dirty && selected !== "new")} title="Save ({modKey}+Enter)">
          {selected === "new" ? "Create snippet" : "Save"}
        </button>
      {/if}
    </div>
  </div>
</Dialog>

<style>
  .wrap {
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .head {
    flex: 1;
    min-width: 0;
  }
  .close {
    margin: -3px -6px 0 0;
  }
  .body {
    display: grid;
    grid-template-columns: 230px 1fr;
    height: min(500px, calc(100vh - 170px));
    min-height: 220px;
    border-top: 1px solid var(--border-subtle);
  }
  .body.narrow {
    grid-template-columns: 1fr;
  }
  .side {
    display: flex;
    flex-direction: column;
    min-height: 0;
    border-right: 1px solid var(--border-subtle);
    background: var(--bg-subtle);
  }
  .narrow .side {
    border-right: 0;
  }
  .side-top {
    display: flex;
    gap: 6px;
    padding: 10px;
  }
  .filter,
  .hosts-filter {
    position: relative;
    display: flex;
    align-items: center;
    flex: 1;
    min-width: 0;
  }
  .filter :global(.f-icon),
  .hosts-filter :global(.f-icon) {
    position: absolute;
    left: 8px;
    color: var(--text-subtle);
    pointer-events: none;
  }
  .filter input {
    width: 100%;
    height: var(--control-h-sm);
    padding: 0 8px 0 26px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-input);
    font-size: var(--text-sm);
  }
  .filter input:focus,
  .hosts-filter input:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .items {
    flex: 1;
    min-height: 0;
    margin: 0;
    padding: 0 6px 8px;
    overflow-y: auto;
    list-style: none;
  }
  .item {
    border-radius: var(--radius-sm);
  }
  .item + .item {
    margin-top: 1px;
  }
  .item:hover:not(.active) {
    background: var(--bg-hover);
  }
  .item.active {
    background: var(--bg-active);
  }
  .item.draft {
    display: flex;
    flex-direction: column;
    padding: 6px 10px;
  }
  .item-btn {
    display: flex;
    flex-direction: column;
    width: 100%;
    padding: 6px 10px;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    text-align: left;
  }
  .item-btn:focus-visible {
    outline-offset: -2px;
  }
  .item-top {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .item-top :global(.run-icon) {
    flex: none;
    color: var(--text-subtle);
  }
  .item-name {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-weight: var(--weight-medium);
  }
  .item-sub {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--text-subtle);
    font-size: var(--text-xs);
  }
  .none {
    padding: 10px;
    color: var(--text-subtle);
    font-size: var(--text-sm);
  }

  .editor {
    display: flex;
    flex-direction: column;
    gap: 16px;
    min-height: 0;
    padding: 16px 20px 20px;
    overflow-y: auto;
  }
  .back {
    align-self: flex-start;
    margin: -6px 0 -6px -8px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 5px;
    min-width: 0;
    margin: 0;
    padding: 0;
    border: 0;
  }
  label,
  legend {
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
  }
  legend {
    padding: 0;
    margin-bottom: 5px;
  }
  .cmd {
    height: auto;
    min-height: 64px;
    padding: 8px 10px;
    line-height: 1.5;
    resize: vertical;
    white-space: pre;
    overflow-x: auto;
  }
  .help,
  .error {
    font-size: var(--text-xs);
    font-weight: normal;
    line-height: 1.4;
  }
  .help {
    color: var(--text-subtle);
  }
  .error {
    color: var(--danger-text);
  }
  .segmented {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 2px;
    padding: 3px;
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    background: var(--bg-inset);
  }
  .segmented label {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: center;
    height: 28px;
    border-radius: var(--radius-sm);
    color: var(--text-muted);
    font-weight: var(--weight-medium);
  }
  .segmented label:has(input:focus-visible) {
    outline: 2px solid var(--focus-ring);
    outline-offset: 1px;
  }
  .segmented input {
    position: absolute;
    opacity: 0;
    pointer-events: none;
  }
  .segmented label.checked {
    background: var(--bg-elevated);
    color: var(--text);
    box-shadow: var(--shadow-sm), 0 0 0 1px var(--border-subtle);
  }
  .hosts {
    margin-top: 6px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    overflow: hidden;
  }
  .hosts.invalid {
    border-color: var(--danger);
  }
  .hosts-filter {
    border-bottom: 1px solid var(--border-subtle);
  }
  .hosts-filter input {
    flex: 1;
    min-width: 0;
    height: 30px;
    padding: 0 8px 0 26px;
    border: 0;
    background: transparent;
    font-size: var(--text-sm);
  }
  .hosts-filter input:focus {
    box-shadow: none;
  }
  .count {
    flex: none;
    padding: 0 10px;
    color: var(--text-subtle);
    font-size: var(--text-xs);
  }
  .host-list {
    max-height: 168px;
    margin: 0;
    padding: 4px;
    overflow-y: auto;
    list-style: none;
  }
  .host-row {
    display: flex;
    align-items: center;
    gap: 9px;
    height: 28px;
    padding: 0 6px;
    border-radius: var(--radius-sm);
    font-weight: normal;
    font-size: var(--text-base);
  }
  .host-row:hover {
    background: var(--bg-hover);
  }
  .host-row input {
    width: 15px;
    height: 15px;
    margin: 0;
    accent-color: var(--accent);
    flex: none;
  }
  .host-name {
    flex: none;
  }
  .host-detail {
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--text-subtle);
    font-size: 11.5px;
  }
  .run {
    align-items: flex-start;
    color: var(--text);
    font-weight: normal;
  }
  .run input {
    margin-top: 2px;
  }
  .run > span {
    display: flex;
    flex-direction: column;
  }
  .run-label {
    font-size: var(--text-base);
  }
  .unsaved {
    color: var(--text-subtle);
    font-size: var(--text-sm);
  }
</style>
