<!--
  Answers an `editConflict` prompt: a file being edited locally was also
  changed on the server. Choices are a keyboard navigable list (arrows +
  Enter, or the underlined letter). Esc / "Decide later" leaves the session
  in conflict; it can be resolved from the Editing panel.
-->
<script lang="ts">
  import CloudUpload from "@lucide/svelte/icons/cloud-upload";
  import FileDiff from "@lucide/svelte/icons/file-diff";
  import HardDriveDownload from "@lucide/svelte/icons/hard-drive-download";
  import RefreshCwOff from "@lucide/svelte/icons/refresh-cw-off";
  import type { LucideIcon } from "@lucide/svelte";
  import Dialog from "../components/Dialog.svelte";
  import { machineName } from "../files/ops";
  import { edits } from "../stores/edits.svelte";
  import { prompts } from "../stores/prompts.svelte";
  import type { EditConflictResolution, Prompt, PromptPayload } from "../types";
  import { formatBytes, formatExact, formatModified } from "../util/format";
  import { basename, dirname } from "../util/paths";

  let { payload, prompt: p }: { payload: PromptPayload; prompt: Extract<Prompt, { kind: "editConflict" }> } = $props();

  const uid = $derived(`edit-conflict-${payload.id}`);
  const name = $derived(basename(p.remotePath));
  const host = $derived(machineName(payload.hostId));
  const session = $derived(edits.map[p.sessionId]);

  const ago = (secs: number | null) => {
    const s = formatModified(secs);
    return s.charAt(0).toLowerCase() + s.slice(1);
  };

  const localNewer = $derived((p.local.modified ?? 0) > (p.remote.modified ?? 0));
  const remoteNewer = $derived((p.remote.modified ?? 0) > (p.local.modified ?? 0));

  interface Choice {
    value: EditConflictResolution;
    label: string;
    key: string;
    detail: string;
    icon: LucideIcon;
  }

  const choices = $derived<Choice[]>([
    {
      value: "overwrite",
      label: "Upload my version",
      key: "u",
      icon: CloudUpload,
      detail: `Replace the file on ${host}. The changes made there are lost.`,
    },
    {
      value: "discardLocal",
      label: "Use the server’s version",
      key: "s",
      icon: HardDriveDownload,
      detail: "Download it into the file your editor has open. Your changes are lost.",
    },
    {
      value: "keepLocal",
      label: "Keep both, stop syncing",
      key: "k",
      icon: RefreshCwOff,
      detail: "Leave the server alone and keep your copy on this computer to merge by hand.",
    },
  ]);

  let list = $state<HTMLElement>();
  let sent = false;

  function send(resolution: EditConflictResolution) {
    if (sent) return;
    sent = true;
    void prompts.respond(payload.id, { kind: "editConflict", resolution });
  }

  function later() {
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
    else {
      const hit = choices.find((c) => c.key === e.key.toLowerCase());
      if (!hit) return;
      send(hit.value);
    }
    e.preventDefault();
  }
</script>

<Dialog onclose={later} labelledby="{uid}-title" describedby="{uid}-desc" width={520} dismissable={false}>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="wrap" {onkeydown}>
    <div class="dialog-header">
      <span class="dialog-icon warning"><FileDiff size={18} /></span>
      <div class="titles">
        <h2 class="dialog-title" id="{uid}-title">“{name}” changed on {host}</h2>
        <p class="dialog-subtitle" id="{uid}-desc">
          You saved it in your editor, but the file in
          <span class="mono path" title={p.remotePath}>{dirname(p.remotePath) ?? "/"}</span>
          was changed since you opened it. Nothing has been uploaded yet.
        </p>
      </div>
    </div>
    <div class="dialog-body">
      <div class="compare">
        <div class="side">
          <div class="side-label">Your version</div>
          <div class="stat"><span class="v">{formatBytes(p.local.size)}</span></div>
          <div class="stat" title={formatExact(p.local.modified)}>
            <span class="v muted">Saved {ago(p.local.modified)}</span>
            {#if localNewer}<span class="badge success">Newer</span>{/if}
          </div>
        </div>
        <div class="side">
          <div class="side-label">On {host}</div>
          <div class="stat"><span class="v">{formatBytes(p.remote.size)}</span></div>
          <div class="stat" title={formatExact(p.remote.modified)}>
            <span class="v muted">Changed {ago(p.remote.modified)}</span>
            {#if remoteNewer}<span class="badge success">Newer</span>{/if}
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
      {#if session}
        <p class="local-note">Your copy: <span class="mono selectable" title={session.localPath}>{session.localPath}</span></p>
      {/if}
    </div>
    <div class="dialog-footer">
      <span class="hint">Undecided saves stay on this computer.</span>
      <span class="spacer"></span>
      <button type="button" class="btn" onclick={later} title="Esc — resolve later from the Editing panel">Decide later</button>
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
    min-width: 0;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--bg-subtle);
  }
  .side-label {
    margin-bottom: 4px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
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
  .local-note {
    margin-top: 10px;
    color: var(--text-muted);
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
  }
  .local-note .mono {
    font-size: 11px;
    color: var(--text);
  }
  .hint {
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
</style>
