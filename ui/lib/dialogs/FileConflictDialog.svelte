<!--
  Answers a `fileConflict` prompt from a transfer. Choices are a keyboard
  navigable list (arrows + Enter, or the underlined letter); "Apply to all"
  remembers the choice for the rest of the transfer. Esc cancels the transfer.
-->
<script lang="ts">
  import ArrowDownToLine from "@lucide/svelte/icons/arrow-down-to-line";
  import CopyPlus from "@lucide/svelte/icons/copy-plus";
  import FileWarning from "@lucide/svelte/icons/file-warning";
  import Replace from "@lucide/svelte/icons/replace";
  import SkipForward from "@lucide/svelte/icons/skip-forward";
  import Sparkles from "@lucide/svelte/icons/sparkles";
  import type { LucideIcon } from "@lucide/svelte";
  import Dialog from "../components/Dialog.svelte";
  import { machineName } from "../files/ops";
  import { prompts } from "../stores/prompts.svelte";
  import { transfers } from "../stores/transfers.svelte";
  import type { ConflictResolution, Prompt, PromptPayload } from "../types";
  import { formatBytes, formatExact, formatModified } from "../util/format";
  import { basename, dirname, extension } from "../util/paths";

  let { payload, prompt: p }: { payload: PromptPayload; prompt: Extract<Prompt, { kind: "fileConflict" }> } = $props();

  const uid = $derived(`conflict-${payload.id}`);
  const name = $derived(basename(p.path));
  const transfer = $derived(transfers.map[p.transferId]);
  const pending = $derived(prompts.queue.filter((q) => q.prompt.kind === "fileConflict").length);

  const srcNewer = $derived((p.source.modified ?? 0) > (p.existing.modified ?? 0));
  const dstNewer = $derived((p.existing.modified ?? 0) > (p.source.modified ?? 0));
  const srcLarger = $derived(p.source.size > p.existing.size);
  const dstLarger = $derived(p.existing.size > p.source.size);

  const keepBothName = $derived.by(() => {
    const ext = extension(name);
    const stem = ext ? name.slice(0, -(ext.length + 1)) : name;
    return ext ? `${stem} (1).${ext}` : `${stem} (1)`;
  });

  interface Choice {
    value: ConflictResolution;
    label: string;
    key: string;
    detail: string;
    icon: LucideIcon;
  }

  const choices = $derived.by((): Choice[] => {
    const out: Choice[] = [];
    if (p.canResume) {
      out.push({
        value: "resume",
        label: "Resume",
        key: "r",
        icon: ArrowDownToLine,
        detail: `Continue the partial file (${formatBytes(p.existing.size)} of ${formatBytes(p.source.size)} already there)`,
      });
    }
    out.push(
      { value: "overwrite", label: "Overwrite", key: "o", icon: Replace, detail: "Replace the existing file" },
      {
        value: "overwriteIfNewer",
        label: "Overwrite if newer",
        key: "n",
        icon: Sparkles,
        detail: srcNewer ? "The incoming file is newer, so it will replace this one" : "The existing file is as new or newer, so it stays",
      },
      { value: "rename", label: "Keep both", key: "k", icon: CopyPlus, detail: `Save the incoming file as “${keepBothName}”` },
      { value: "skip", label: "Skip", key: "s", icon: SkipForward, detail: "Leave the existing file untouched" },
    );
    return out;
  });

  let applyToAll = $state(false);
  let list = $state<HTMLElement>();
  let sent = false;

  function send(resolution: ConflictResolution) {
    if (sent) return;
    sent = true;
    void prompts.respond(payload.id, { kind: "conflict", resolution, applyToAll });
  }

  function cancel() {
    if (sent) return;
    sent = true;
    void prompts.respond(payload.id, { kind: "cancel" });
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    const target = e.target as HTMLElement;
    const buttons = list ? [...list.querySelectorAll<HTMLElement>("[data-choice]")] : [];
    const i = buttons.indexOf(target);
    if (e.key === "ArrowDown" && i !== -1) buttons[(i + 1) % buttons.length]?.focus();
    else if (e.key === "ArrowUp" && i !== -1) buttons[(i - 1 + buttons.length) % buttons.length]?.focus();
    else if (e.key.toLowerCase() === "a") applyToAll = !applyToAll;
    else {
      const hit = choices.find((c) => c.key === e.key.toLowerCase());
      if (!hit) return;
      send(hit.value);
    }
    e.preventDefault();
  }
</script>

<Dialog onclose={cancel} labelledby="{uid}-title" describedby="{uid}-desc" width={520} dismissable={false}>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="wrap" {onkeydown}>
    <div class="dialog-header">
      <span class="dialog-icon warning"><FileWarning size={18} /></span>
      <div class="titles">
        <h2 class="dialog-title" id="{uid}-title">“{name}” already exists</h2>
        <p class="dialog-subtitle" id="{uid}-desc">
          In <span class="mono path" title={p.path}>{dirname(p.path) ?? "/"}</span> on {machineName(payload.hostId)}
          {#if transfer}· while copying {transfer.label}{/if}
        </p>
      </div>
    </div>
    <div class="dialog-body">
      <div class="compare">
        <div class="side">
          <div class="side-label">Incoming</div>
          <div class="stat">
            <span class="v">{formatBytes(p.source.size)}</span>
            {#if srcLarger}<span class="badge accent">Larger</span>{/if}
          </div>
          <div class="stat" title={formatExact(p.source.modified)}>
            <span class="v muted">{formatModified(p.source.modified)}</span>
            {#if srcNewer}<span class="badge success">Newer</span>{/if}
          </div>
        </div>
        <div class="side">
          <div class="side-label">Existing</div>
          <div class="stat">
            <span class="v">{formatBytes(p.existing.size)}</span>
            {#if dstLarger}<span class="badge accent">Larger</span>{/if}
          </div>
          <div class="stat" title={formatExact(p.existing.modified)}>
            <span class="v muted">{formatModified(p.existing.modified)}</span>
            {#if dstNewer}<span class="badge success">Newer</span>{/if}
          </div>
        </div>
      </div>

      <div class="choices" bind:this={list} role="group" aria-label="What to do">
        {#each choices as c, i (c.value)}
          <button type="button" class="choice" data-choice data-autofocus={i === 0 ? true : undefined} onclick={() => send(c.value)}>
            <span class="c-icon"><c.icon size={15} /></span>
            <span class="c-text">
              <span class="c-label">{c.label}</span>
              <span class="c-detail">{c.detail}</span>
            </span>
            <kbd>{c.key.toUpperCase()}</kbd>
          </button>
        {/each}
      </div>
      <p class="esc-note"><kbd>Esc</kbd> or <strong>Cancel transfer</strong> stops the whole transfer, including files not copied yet.</p>
    </div>
    <div class="dialog-footer">
      <label class="checkbox all">
        <input type="checkbox" bind:checked={applyToAll} />
        <span>Apply to <u>a</u>ll remaining conflicts</span>{#if pending > 1}<span class="count"> ({pending - 1} more waiting)</span>{/if}
      </label>
      <span class="spacer"></span>
      <button type="button" class="btn" onclick={cancel} title="Esc — stops the whole transfer">Cancel transfer</button>
    </div>
  </div>
</Dialog>

<style>
  .wrap {
    display: contents;
  }
  .titles {
    min-width: 0;
  }
  .dialog-title {
    overflow-wrap: anywhere;
  }
  .path {
    font-size: 11.5px;
    word-break: break-all;
  }
  .compare {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px;
  }
  .side {
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--bg-subtle);
  }
  .side-label {
    margin-bottom: 4px;
    color: var(--text-subtle);
    font-size: var(--text-xs);
    font-weight: var(--weight-semibold);
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  .stat {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 22px;
  }
  .v {
    font-weight: var(--weight-medium);
    font-variant-numeric: tabular-nums;
  }
  .v.muted {
    color: var(--text-muted);
    font-weight: normal;
    font-size: var(--text-sm);
  }
  .choices {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin-top: 14px;
  }
  .choice {
    display: flex;
    align-items: center;
    gap: 12px;
    width: 100%;
    padding: 8px 10px;
    border: 1px solid transparent;
    border-radius: var(--radius-md);
    background: transparent;
    text-align: left;
    color: var(--text);
  }
  .choice:hover {
    background: var(--bg-hover);
  }
  .choice:focus-visible,
  .choice:focus {
    outline: none;
    background: var(--accent-soft);
    border-color: var(--accent);
  }
  .c-icon {
    display: grid;
    place-items: center;
    flex: none;
    width: 30px;
    height: 30px;
    border-radius: var(--radius-sm);
    background: var(--bg-subtle);
    color: var(--text-muted);
  }
  .choice:focus .c-icon {
    background: var(--bg-elevated);
    color: var(--accent-text);
  }
  .c-text {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
  }
  .c-label {
    font-weight: var(--weight-medium);
  }
  .c-detail {
    color: var(--text-muted);
    font-size: var(--text-sm);
    overflow-wrap: anywhere;
  }
  .esc-note {
    margin-top: 10px;
    color: var(--text-muted);
    font-size: var(--text-xs);
  }
  .esc-note strong {
    font-weight: var(--weight-medium);
    color: var(--text);
  }
  .all {
    font-size: var(--text-sm);
    color: var(--text);
  }
  .count {
    color: var(--text-muted);
  }
</style>
