<!-- Create / edit an app-managed host, or view a read-only ssh_config host. -->
<script lang="ts">
  import CopyPlus from "@lucide/svelte/icons/copy-plus";
  import FileText from "@lucide/svelte/icons/file-text";
  import FolderOpen from "@lucide/svelte/icons/folder-open";
  import Plus from "@lucide/svelte/icons/plus";
  import Server from "@lucide/svelte/icons/server";
  import X from "@lucide/svelte/icons/x";
  import { tick, untrack } from "svelte";
  import { duplicateHost } from "../actions";
  import { isTauri } from "../api";
  import ChipInput from "../components/ChipInput.svelte";
  import Combobox from "../components/Combobox.svelte";
  import Dialog from "../components/Dialog.svelte";
  import HostDetails from "../components/HostDetails.svelte";
  import { hosts } from "../stores/hosts.svelte";
  import { tabs } from "../stores/tabs.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { ui, type EditorState } from "../stores/ui.svelte";
  import type { HostDraft } from "../types";
  import { toCommandError, sentence } from "../util/errors";
  import { address, jumpHops } from "../util/format";

  let { editor }: { editor: EditorState } = $props();

  const existing = $derived(editor.mode === "create" ? undefined : hosts.get(editor.hostId));
  const readOnly = $derived(editor.mode === "view");

  interface Form {
    name: string;
    hostname: string;
    port: string;
    user: string;
    identityFiles: string[];
    proxyJump: string;
    group: string;
    tags: string[];
  }

  function initialForm(): Form {
    const src: Partial<HostDraft> =
      editor.mode === "create" ? (editor.initial ?? {}) : (hosts.get(editor.hostId) ?? {});
    return {
      name: src.name ?? "",
      hostname: src.hostname ?? "",
      port: src.port && src.port !== 22 ? String(src.port) : "",
      user: src.user ?? "",
      identityFiles: [...(src.identityFiles ?? [])],
      proxyJump: src.proxyJump ?? "",
      group: src.group ?? "",
      tags: [...(src.tags ?? [])],
    };
  }

  const initial = untrack(initialForm);
  let form = $state<Form>(structuredClone(initial));
  let submitted = $state(false);
  let saving = $state(false);
  let saveError = $state<string | null>(null);
  let filesEl = $state<HTMLElement>();

  const selfId = $derived(editor.mode === "edit" ? editor.hostId : null);
  const dirty = $derived(JSON.stringify(form) !== JSON.stringify(initial));

  const errors = $derived.by(() => {
    const e: Partial<Record<keyof Form, string>> = {};
    const name = form.name.trim();
    if (!name) e.name = "Give the host a name.";
    else if (/[\s,]/.test(name)) e.name = "No spaces or commas — the name can be used in ProxyJump.";
    else {
      const clash = hosts.findByName(name);
      if (clash && clash.id !== selfId) e.name = `Another host is already called “${clash.name}”.`;
    }
    const hostname = form.hostname.trim();
    if (!hostname) e.hostname = "Enter a hostname or IP address.";
    else if (/\s/.test(hostname)) e.hostname = "Hostnames can’t contain spaces.";
    else if (hostname.includes("@")) e.hostname = "Put the user name in the User field.";
    const port = form.port.trim();
    if (port && (!/^\d+$/.test(port) || +port < 1 || +port > 65535))
      e.port = "Use a port between 1 and 65535.";
    if (/[\s@]/.test(form.user.trim())) e.user = "User names can’t contain spaces or @.";
    const hops = form.proxyJump.split(",").map((s) => s.trim());
    if (form.proxyJump.trim()) {
      if (hops.some((h) => !h)) e.proxyJump = "Remove the empty entry between commas.";
      else if (hops.some((h) => /\s/.test(h))) e.proxyJump = "Separate hops with commas, without spaces inside a hop.";
      else if (name && hops.some((h) => h.toLowerCase() === name.toLowerCase()))
        e.proxyJump = "A host can’t jump through itself.";
    }
    return e;
  });
  const valid = $derived(Object.keys(errors).length === 0);
  // Errors appear after the first save attempt, then update live. (Showing
  // them on blur shifts the layout under the pointer mid-click.)
  const show = (field: keyof Form) => (submitted ? errors[field] : undefined);

  const jumpOptions = $derived(hosts.list.filter((h) => h.id !== selfId).map((h) => h.name));
  const describeHost = (name: string) => {
    const h = hosts.findByName(name);
    return h ? address(h) : undefined;
  };

  function toDraft(): HostDraft {
    const opt = (s: string) => s.trim() || null;
    return {
      name: form.name.trim(),
      hostname: form.hostname.trim(),
      port: form.port.trim() ? Number(form.port) : null,
      user: opt(form.user),
      identityFiles: form.identityFiles.map((f) => f.trim()).filter(Boolean),
      proxyJump: jumpHops(form.proxyJump).join(",") || null,
      group: opt(form.group),
      tags: form.tags,
    };
  }

  async function onsubmit(e: SubmitEvent) {
    e.preventDefault();
    if (readOnly) return;
    submitted = true;
    saveError = null;
    if (!valid) {
      await tick();
      document.querySelector<HTMLElement>(".editor [aria-invalid='true']")?.focus();
      return;
    }
    saving = true;
    try {
      const draft = toDraft();
      if (editor.mode === "edit") {
        const host = await hosts.update(editor.hostId, draft);
        toasts.success(`Saved ${host.name}`);
      } else {
        const host = await hosts.create(draft);
        tabs.openHost(host.id);
        toasts.success(`Added ${host.name}`);
      }
      ui.editor = null;
    } catch (err) {
      saveError = sentence(toCommandError(err).message);
    } finally {
      saving = false;
    }
  }

  async function close() {
    if (!readOnly && dirty && !saving) {
      const discard = await ui.confirm({
        title: "Discard changes?",
        message: "Your changes to this host haven’t been saved.",
        confirmLabel: "Discard",
        danger: true,
      });
      if (!discard) return;
    }
    ui.editor = null;
  }

  async function addFile() {
    form.identityFiles = [...form.identityFiles, ""];
    await tick();
    filesEl?.querySelector<HTMLInputElement>("li:last-child input")?.focus();
  }

  async function browse() {
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const { homeDir, join } = await import("@tauri-apps/api/path");
      const picked = await open({
        title: "Choose private key",
        multiple: true,
        directory: false,
        defaultPath: await join(await homeDir(), ".ssh"),
      });
      const paths = picked === null ? [] : Array.isArray(picked) ? picked : [picked];
      if (paths.length) {
        const kept = form.identityFiles.filter((f) => f.trim());
        form.identityFiles = [...new Set([...kept, ...paths])];
      }
    } catch (err) {
      toasts.error(toCommandError(err), "Couldn't open file picker");
    }
  }

  function duplicate() {
    if (existing) duplicateHost(existing);
  }

  const title = $derived(
    editor.mode === "create" ? "Add host" : editor.mode === "edit" ? `Edit ${existing?.name ?? "host"}` : (existing?.name ?? "Host"),
  );
</script>

{#snippet fieldError(field: keyof Form, help?: string)}
  {#if show(field)}
    <p class="error" id="f-{field}-msg">{show(field)}</p>
  {:else if help}
    <p class="help" id="f-{field}-msg">{help}</p>
  {/if}
{/snippet}

<Dialog onclose={close} labelledby="editor-title" width={readOnly ? 500 : 560} dismissable={readOnly}>
  {#if readOnly && existing}
    <div class="dialog-header">
      <span class="dialog-icon"><FileText size={18} /></span>
      <div>
        <h2 class="dialog-title" id="editor-title">{existing.name}</h2>
        <p class="dialog-subtitle">Defined in <code>~/.ssh/config</code></p>
      </div>
    </div>
    <div class="dialog-body">
      <p class="readonly-note">
        Hosts from your SSH config are read-only in Patchbay. Edit <code>~/.ssh/config</code> to change this one, or duplicate it
        as a Patchbay host you can edit here.
      </p>
      <HostDetails host={existing} linkHops={false} />
    </div>
    <div class="dialog-footer">
      <span class="spacer"></span>
      <button type="button" class="btn" onclick={() => (ui.editor = null)} data-autofocus>Close</button>
      <button type="button" class="btn btn-primary" onclick={duplicate}><CopyPlus size={14} /> Duplicate as Patchbay host</button>
    </div>
  {:else}
    <form class="editor" {onsubmit} novalidate>
      <div class="dialog-header">
        <span class="dialog-icon"><Server size={18} /></span>
        <div>
          <h2 class="dialog-title" id="editor-title">{title}</h2>
          <p class="dialog-subtitle">
            {editor.mode === "create" ? "Saved in Patchbay’s own host list." : "Changes apply to the next connection."}
          </p>
        </div>
      </div>

      <div class="dialog-body">
        <div class="group" role="group" aria-labelledby="g-conn">
          <h3 class="group-title" id="g-conn">Connection</h3>
          <div class="grid">
            <div class="field span-full">
              <label for="f-name">Name</label>
              <input
                id="f-name"
                class="input"
                bind:value={form.name}
                placeholder="e.g. homelab-pi"
                autocomplete="off"
                spellcheck="false"
                aria-invalid={!!show("name")}
                aria-describedby="f-name-msg"
                data-autofocus
              />
              {@render fieldError("name")}
            </div>
            <div class="field span-host">
              <label for="f-hostname">Hostname</label>
              <input
                id="f-hostname"
                class="input mono"
                bind:value={form.hostname}
                placeholder="server.example.com or 10.0.0.5"
                autocomplete="off"
                spellcheck="false"
                aria-invalid={!!show("hostname")}
                aria-describedby="f-hostname-msg"
              />
              {@render fieldError("hostname")}
            </div>
            <div class="field">
              <label for="f-port">Port</label>
              <input
                id="f-port"
                class="input mono"
                bind:value={form.port}
                placeholder="22"
                inputmode="numeric"
                autocomplete="off"
                aria-invalid={!!show("port")}
                aria-describedby="f-port-msg"
              />
              {@render fieldError("port")}
            </div>
            <div class="field span-full">
              <label for="f-user">User</label>
              <input
                id="f-user"
                class="input mono"
                bind:value={form.user}
                placeholder="Defaults to your local user name"
                autocomplete="off"
                spellcheck="false"
                aria-invalid={!!show("user")}
                aria-describedby="f-user-msg"
              />
              {@render fieldError("user")}
            </div>
          </div>
        </div>

        <div class="group" role="group" aria-labelledby="f-keys-label">
          <h3 class="group-title" id="f-keys-label">Identity files</h3>
          {#if form.identityFiles.length}
            <ul class="files" bind:this={filesEl} aria-labelledby="f-keys-label">
              {#each form.identityFiles as _, i (i)}
                <li>
                  <input
                    class="input mono"
                    bind:value={form.identityFiles[i]}
                    placeholder="~/.ssh/id_ed25519"
                    aria-label="Identity file {i + 1}"
                    autocomplete="off"
                    spellcheck="false"
                  />
                  <button
                    type="button"
                    class="icon-btn"
                    aria-label="Remove identity file {i + 1}"
                    title="Remove"
                    onclick={() => (form.identityFiles = form.identityFiles.filter((_, j) => j !== i))}
                  >
                    <X size={14} />
                  </button>
                </li>
              {/each}
            </ul>
          {:else}
            <p class="help files-empty">None — Patchbay will use your SSH agent and default keys.</p>
          {/if}
          <div class="file-actions">
            <button type="button" class="btn btn-sm" onclick={addFile}><Plus size={13} /> Add path</button>
            {#if isTauri}
              <button type="button" class="btn btn-sm" onclick={browse}><FolderOpen size={13} /> Browse…</button>
            {/if}
          </div>
        </div>

        <div class="group" role="group" aria-labelledby="g-org">
          <h3 class="group-title" id="g-org">Routing &amp; organization</h3>
          <div class="grid">
            <div class="field span-full">
              <label for="f-jump">Proxy jump</label>
              <Combobox
                id="f-jump"
                bind:value={form.proxyJump}
                options={jumpOptions}
                describe={describeHost}
                multi
                mono
                placeholder="None — connect directly"
                invalid={!!show("proxyJump")}
                describedby="f-proxyJump-msg"
              />
              {@render fieldError("proxyJump", "Hosts to hop through, comma-separated: a host name or user@host:port.")}
            </div>
            <div class="field span-group">
              <label for="f-group">Group</label>
              <Combobox
                id="f-group"
                bind:value={form.group}
                options={hosts.groups}
                placeholder="No group"
              />
            </div>
            <div class="field span-tags">
              <label for="f-tags">Tags</label>
              <ChipInput id="f-tags" bind:values={form.tags} suggestions={hosts.tags} placeholder="Add tags…" />
            </div>
          </div>
        </div>
      </div>

      <div class="dialog-footer">
        {#if saveError}
          <p class="save-error" role="alert">{saveError}</p>
        {/if}
        <span class="spacer"></span>
        <button type="button" class="btn" onclick={close}>Cancel</button>
        <button type="submit" class="btn btn-primary" disabled={saving}>
          {editor.mode === "create" ? "Add host" : "Save changes"}
        </button>
      </div>
    </form>
  {/if}
</Dialog>

<style>
  .editor {
    display: contents;
  }
  .group {
    min-width: 0;
  }
  .group + .group {
    margin-top: 18px;
    padding-top: 16px;
    border-top: 1px solid var(--border-subtle);
  }
  .group-title {
    margin: 0 0 10px;
    color: var(--text-muted);
    font-size: var(--text-xs);
    font-weight: var(--weight-semibold);
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  .grid {
    display: grid;
    grid-template-columns: 1fr 1fr 110px;
    gap: 12px;
  }
  .span-host {
    grid-column: span 2;
  }
  .span-full {
    grid-column: 1 / -1;
  }
  .span-group {
    grid-column: span 1;
  }
  .span-tags {
    grid-column: span 2;
  }
  @media (max-width: 600px) {
    .grid {
      grid-template-columns: 1fr 90px;
    }
    .span-tags,
    .span-group {
      grid-column: 1 / -1;
    }
    .span-host {
      grid-column: span 1;
    }
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 5px;
    min-width: 0;
  }
  label {
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
  }
  .help,
  .error {
    font-size: var(--text-xs);
    line-height: 1.4;
  }
  .help {
    color: var(--text-subtle);
  }
  .error {
    color: var(--danger-text);
  }
  .files {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin: 0 0 8px;
    padding: 0;
    list-style: none;
  }
  .files li {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .files .icon-btn {
    width: 30px;
    height: 30px;
  }
  .files-empty {
    margin-bottom: 8px;
    font-size: var(--text-sm);
  }
  .file-actions {
    display: flex;
    gap: 6px;
  }
  .save-error {
    flex: 1 1 auto;
    min-width: 0;
    color: var(--danger-text);
    font-size: var(--text-sm);
  }
  .save-error + .spacer {
    display: none;
  }
  .readonly-note {
    margin-bottom: 8px;
    padding: 10px 12px;
    border-radius: var(--radius-md);
    background: var(--bg-subtle);
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
</style>
