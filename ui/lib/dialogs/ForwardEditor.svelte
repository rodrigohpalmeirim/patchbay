<!-- Create / edit a port forward rule. -->
<script lang="ts">
  import Cable from "@lucide/svelte/icons/cable";
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";
  import { tick, untrack } from "svelte";
  import Dialog from "../components/Dialog.svelte";
  import Segmented from "../components/Segmented.svelte";
  import RouteDiagram from "../forwards/RouteDiagram.svelte";
  import { hostPort, isAllInterfaces, isLoopback, KIND_FLAG } from "../forwards/ops";
  import { forwards, isRunning } from "../stores/forwards.svelte";
  import { hosts } from "../stores/hosts.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { ui, type ForwardEditorState } from "../stores/ui.svelte";
  import type { Host, HostId, PortForwardDraft, PortForwardKind } from "../types";
  import { sentence, toCommandError } from "../util/errors";
  import { address, collator } from "../util/format";
  import { hasMod, modKey } from "../util/platform";

  let { editor }: { editor: ForwardEditorState } = $props();

  type BindMode = "loopback" | "all" | "custom";

  interface Form {
    name: string;
    kind: PortForwardKind;
    hostId: string;
    bindMode: BindMode;
    bindCustom: string;
    bindPort: string;
    targetHost: string;
    targetPort: string;
    from: "here" | "host";
    viaHostId: string;
    autoStart: boolean;
  }

  const existing = $derived(editor.mode === "edit" ? forwards.get(editor.id) : undefined);

  function initialForm(): Form {
    const src: Partial<PortForwardDraft> =
      editor.mode === "create" ? (editor.initial ?? {}) : (forwards.get(editor.id) ?? {});
    const bind = src.bindAddress ?? null;
    const kind = src.kind ?? "local";
    return {
      name: src.name ?? "",
      kind,
      hostId: src.hostId ?? "",
      bindMode: isLoopback(bind) ? "loopback" : isAllInterfaces(bind) ? "all" : "custom",
      bindCustom: !isLoopback(bind) && !isAllInterfaces(bind) ? (bind ?? "") : "",
      bindPort: src.bindPort ? String(src.bindPort) : "",
      targetHost: src.targetHost ?? (kind === "dynamic" ? "" : "localhost"),
      targetPort: src.targetPort ? String(src.targetPort) : "",
      from: src.viaHostId ? "host" : "here",
      viaHostId: src.viaHostId ?? "",
      autoStart: src.autoStart ?? false,
    };
  }

  const initial = untrack(initialForm);
  let form = $state<Form>(structuredClone(initial));
  let submitted = $state(false);
  let saving = $state(false);
  let saveError = $state<string | null>(null);
  let startAfter = $state(untrack(() => editor.mode === "create"));
  /** The name was filled in by a preset and may be replaced by the next one. */
  let autoName = $state(untrack(() => !form.name));

  const dirty = $derived(JSON.stringify(form) !== JSON.stringify(initial));
  const selfId = $derived(editor.mode === "edit" ? editor.id : null);
  const running = $derived(selfId ? isRunning(forwards.status(selfId)) : false);

  // ------------------------------------------------------------- hosts

  const hostGroups = $derived.by(() => {
    const sorted = [...hosts.list].sort((a, b) => collator.compare(a.name, b.name));
    const out: { label: string; hosts: Host[] }[] = [];
    const ungrouped = sorted.filter((h) => h.source !== "tailscale" && !h.group);
    if (ungrouped.length) out.push({ label: "Hosts", hosts: ungrouped });
    const groups = new Map<string, Host[]>();
    for (const h of sorted.filter((h) => h.source !== "tailscale" && h.group)) {
      groups.set(h.group!, [...(groups.get(h.group!) ?? []), h]);
    }
    for (const [label, list] of [...groups].sort(([a], [b]) => collator.compare(a, b))) out.push({ label, hosts: list });
    const tailnet = sorted.filter((h) => h.source === "tailscale");
    if (tailnet.length) out.push({ label: "Tailscale", hosts: tailnet });
    return out;
  });

  const host = $derived(hosts.get(form.hostId));
  const via = $derived(form.kind === "remote" && form.from === "host" ? hosts.get(form.viaHostId) : undefined);
  const hostLabel = $derived(host?.name ?? "the SSH host");

  // If the carrying host changes to the "connect from" host, clear the latter.
  $effect(() => {
    if (form.viaHostId && form.viaHostId === form.hostId) untrack(() => (form.viaHostId = ""));
  });

  // ------------------------------------------------------------ presets

  interface Preset {
    label: string;
    port: number;
    /** Suggested local port when the service port is privileged. */
    local?: number;
  }
  const PRESETS: Preset[] = [
    { label: "PostgreSQL", port: 5432 },
    { label: "MySQL", port: 3306 },
    { label: "Redis", port: 6379 },
    { label: "MongoDB", port: 27017 },
    { label: "HTTP", port: 80, local: 8080 },
    { label: "HTTP alt", port: 8080 },
    { label: "HTTPS", port: 443, local: 8443 },
    { label: "Kubernetes API", port: 6443 },
    { label: "RDP", port: 3389 },
    { label: "VNC", port: 5900 },
    { label: "SSH", port: 22, local: 2222 },
  ];

  function applyPreset(p: Preset) {
    form.targetPort = String(p.port);
    if (!form.targetHost.trim()) form.targetHost = "localhost";
    if (!form.bindPort.trim() || PRESETS.some((x) => String(x.local ?? x.port) === form.bindPort.trim())) {
      form.bindPort = String(p.local ?? (p.port < 1024 ? 8000 + p.port : p.port));
    }
    if (autoName || !form.name.trim()) {
      form.name = forwards.uniqueName(host ? `${p.label} on ${host.name}` : p.label);
      autoName = true;
    }
  }

  const presetActive = (p: Preset) => form.targetPort.trim() === String(p.port);

  // --------------------------------------------------------- validation

  const portOf = (v: string) => (/^\d+$/.test(v.trim()) ? Number(v.trim()) : NaN);

  const bindAddress = $derived(
    form.bindMode === "loopback" ? null : form.bindMode === "all" ? "0.0.0.0" : form.bindCustom.trim() || null,
  );
  const exposed = $derived(form.bindMode === "all" || (form.bindMode === "custom" && !isLoopback(form.bindCustom.trim() || null)));
  const autoPort = $derived(form.kind !== "remote" && (form.bindPort.trim() === "" || form.bindPort.trim() === "0"));

  const errors = $derived.by(() => {
    const e: Partial<Record<keyof Form, string>> = {};
    const name = form.name.trim();
    if (!name) e.name = "Give the forward a name.";
    else {
      const clash = forwards.list.find((f) => f.id !== selfId && f.name.toLowerCase() === name.toLowerCase());
      if (clash) e.name = "Another forward already has this name.";
    }
    if (!form.hostId) e.hostId = "Choose the SSH host to forward through.";
    else if (!host) e.hostId = "This host no longer exists.";
    if (form.bindMode === "custom") {
      const a = form.bindCustom.trim();
      if (!a) e.bindCustom = "Enter an address to listen on, e.g. 192.168.1.10.";
      else if (/\s|\/|^\[?[^\]]*\]?:\d+$/.test(a) && !a.includes("::")) e.bindCustom = "Just the address, without a port.";
    }
    const bp = form.bindPort.trim();
    if (form.kind === "remote") {
      if (!bp) e.bindPort = "Choose the port to open on the server.";
      else if (!(portOf(bp) >= 1 && portOf(bp) <= 65535)) e.bindPort = "Use a port between 1 and 65535.";
    } else if (bp && !(portOf(bp) >= 0 && portOf(bp) <= 65535)) {
      e.bindPort = "Use a port between 1 and 65535, or leave empty for any free port.";
    }
    if (form.kind !== "dynamic") {
      const th = form.targetHost.trim();
      if (!th) e.targetHost = "Enter the host to connect to.";
      else if (/^[a-z][a-z0-9+.-]*:\/\//i.test(th)) e.targetHost = "Enter just the host name, without http:// or a path.";
      else if (/[\s/]/.test(th)) e.targetHost = "Host names can’t contain spaces or slashes.";
      const tp = form.targetPort.trim();
      if (!tp) e.targetPort = "Enter the port.";
      else if (!(portOf(tp) >= 1 && portOf(tp) <= 65535)) e.targetPort = "Use 1–65535.";
    }
    if (form.kind === "remote" && form.from === "host") {
      if (!form.viaHostId) e.viaHostId = "Choose the host that connects to the target.";
      else if (!hosts.get(form.viaHostId)) e.viaHostId = "This host no longer exists.";
    }
    return e;
  });
  const valid = $derived(Object.keys(errors).length === 0);
  const show = (field: keyof Form) => (submitted ? errors[field] : undefined);

  /** Soft warnings that don't block saving. */
  const portWarning = $derived.by(() => {
    const p = portOf(form.bindPort);
    if (!Number.isFinite(p) || p === 0) return null;
    if (form.kind === "remote") return p < 1024 ? `Ports below 1024 usually need root on ${hostLabel}.` : null;
    if (p < 1024) return "Ports below 1024 need administrator rights on most systems.";
    const clash = forwards.list.find((f) => f.id !== selfId && f.kind !== "remote" && f.bindPort === p);
    return clash ? `“${clash.name}” also listens on ${p}; only one of them can run at a time.` : null;
  });

  // Paste "host:port" into the target host: split it.
  function splitTarget() {
    const v = form.targetHost.trim();
    const m = /^\[?([^\]]+?)\]?:(\d{1,5})$/.exec(v);
    if (m && !v.includes("::") && !form.targetPort.trim()) {
      form.targetHost = m[1];
      form.targetPort = m[2];
    }
  }

  // ---------------------------------------------------------------- copy

  const KIND_INFO: Record<PortForwardKind, string> = {
    local: "Reach a service behind the SSH host as if it ran on this computer.",
    remote: "Open a port on the SSH host that leads back to this computer — or to another host.",
    dynamic: "A SOCKS5 proxy on this computer; apps using it connect out from the SSH host.",
  };

  const hostFieldLabel = $derived(form.kind === "dynamic" ? "Exit through" : "SSH host");
  const listenTitle = $derived(form.kind === "remote" ? `Listen on ${host ? host.name : "the host"}` : "Listen on this computer");
  const seenFrom = $derived(
    form.kind === "local" ? hostLabel : form.from === "host" ? (via?.name ?? "the chosen host") : "this computer",
  );

  const exposeWarning = $derived.by(() => {
    if (!exposed) return null;
    if (form.kind === "remote")
      return `Anyone who can reach ${hostLabel} can connect to this port. The server must allow it (GatewayPorts in sshd_config).`;
    if (form.kind === "dynamic") return "Other devices on your network can use this proxy to reach anything the SSH host can.";
    return "Other devices on your network can connect to this port.";
  });

  const listenLabel = $derived(
    form.kind === "remote" ? `:${form.bindPort.trim() || "port"}` : autoPort ? ":auto" : `:${form.bindPort.trim()}`,
  );
  const targetLabel = $derived(
    form.kind === "dynamic" ? null : hostPort(form.targetHost.trim() || "target", form.targetPort.trim() || "port"),
  );

  // ---------------------------------------------------------------- save

  function toDraft(): PortForwardDraft {
    const p = portOf(form.bindPort);
    return {
      name: form.name.trim(),
      kind: form.kind,
      hostId: form.hostId as HostId,
      bindAddress,
      bindPort: Number.isFinite(p) ? p : 0,
      targetHost: form.kind === "dynamic" ? null : form.targetHost.trim(),
      targetPort: form.kind === "dynamic" ? null : portOf(form.targetPort),
      viaHostId: form.kind === "remote" && form.from === "host" ? (form.viaHostId as HostId) : null,
      autoStart: form.autoStart,
    };
  }

  async function onsubmit(e?: Event) {
    e?.preventDefault();
    submitted = true;
    saveError = null;
    if (!valid) {
      await tick();
      document.querySelector<HTMLElement>(".fwd-editor [aria-invalid='true']")?.focus();
      return;
    }
    saving = true;
    try {
      const draft = toDraft();
      if (editor.mode === "edit") {
        const f = await forwards.update(editor.id, draft);
        toasts.success(`Saved “${f.name}”`, running ? "The forward restarted with the new settings." : undefined);
        ui.forwardEditor = null;
        if (!running && startAfter) void forwards.start(f.id);
      } else {
        const f = await forwards.create(draft);
        toasts.success(`Added “${f.name}”`);
        ui.forwardEditor = null;
        if (startAfter) void forwards.start(f.id);
      }
    } catch (err) {
      saveError = sentence(toCommandError(err).message);
    } finally {
      saving = false;
    }
  }

  async function close() {
    if (dirty && !saving) {
      const discard = await ui.confirm({
        title: "Discard changes?",
        message: "Your changes to this port forward haven’t been saved.",
        confirmLabel: "Discard",
        danger: true,
      });
      if (!discard) return;
    }
    ui.forwardEditor = null;
  }

  function onkeydown(e: KeyboardEvent) {
    if (hasMod(e) && e.key === "Enter") {
      e.preventDefault();
      void onsubmit();
    }
  }

  const title = $derived(editor.mode === "create" ? "New port forward" : `Edit “${existing?.name ?? "forward"}”`);
</script>

{#snippet fieldError(field: keyof Form, help?: string)}
  {#if show(field)}
    <p class="error" id="fw-{field}-msg">{show(field)}</p>
  {:else if help}
    <p class="help" id="fw-{field}-msg">{help}</p>
  {/if}
{/snippet}

{#snippet hostSelect(id: string, value: string, onchange: (v: string) => void, exclude: string | null, invalid: boolean, placeholder: string)}
  <select {id} class="input select" {value} onchange={(e) => onchange(e.currentTarget.value)} aria-invalid={invalid} aria-describedby="{id}-msg">
    <option value="" disabled>{placeholder}</option>
    {#each hostGroups as g (g.label)}
      <optgroup label={g.label}>
        {#each g.hosts.filter((h) => h.id !== exclude) as h (h.id)}
          <option value={h.id}>{h.name} — {address(h)}</option>
        {/each}
      </optgroup>
    {/each}
  </select>
{/snippet}

<Dialog onclose={close} labelledby="fw-title" width={600} dismissable={false}>
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <form class="fwd-editor" {onsubmit} {onkeydown} novalidate>
    <div class="dialog-header">
      <span class="dialog-icon"><Cable size={18} /></span>
      <div>
        <h2 class="dialog-title" id="fw-title">{title}</h2>
        <p class="dialog-subtitle">{KIND_INFO[form.kind]}</p>
      </div>
    </div>

    <div class="dialog-body">
      <div class="field">
        <label for="fw-name">Name</label>
        <input
          id="fw-name"
          class="input"
          bind:value={form.name}
          oninput={() => (autoName = false)}
          placeholder="e.g. Production database"
          autocomplete="off"
          spellcheck="false"
          aria-invalid={!!show("name")}
          aria-describedby="fw-name-msg"
          data-autofocus
        />
        {@render fieldError("name")}
      </div>

      <div class="field">
        <span class="label" id="fw-kind-label">Type</span>
        <Segmented
          name="fw-kind"
          labelledby="fw-kind-label"
          bind:value={form.kind}
          options={[
            { value: "local", label: "Local", title: `Local forward (ssh ${KIND_FLAG.local})` },
            { value: "remote", label: "Remote", title: `Remote forward (ssh ${KIND_FLAG.remote})` },
            { value: "dynamic", label: "SOCKS proxy", title: `Dynamic forward (ssh ${KIND_FLAG.dynamic})` },
          ]}
        />
      </div>

      <RouteDiagram
        kind={form.kind}
        host={host?.name ?? "SSH host"}
        via={via?.name ?? (form.kind === "remote" && form.from === "host" ? "other host" : null)}
        listen={listenLabel}
        target={targetLabel}
        targetLoopback={isLoopback(form.targetHost.trim() || null)}
        {exposed}
      />

      <div class="grid">
        <div class="field span-full">
          <label for="fw-host">{hostFieldLabel}</label>
          {@render hostSelect("fw-host", form.hostId, (v) => (form.hostId = v), null, !!show("hostId"), "Choose a host…")}
          {@render fieldError("hostId")}
        </div>
      </div>

      <fieldset class="group">
        <legend class="group-title">{listenTitle}</legend>
        <div class="grid">
          <div class="field span-2">
            <span class="label" id="fw-bind-label">Address</span>
            <Segmented
              name="fw-bind"
              size="sm"
              labelledby="fw-bind-label"
              bind:value={form.bindMode}
              options={[
                { value: "loopback", label: form.kind === "remote" ? "Server only" : "This computer only", title: "Loopback (127.0.0.1)" },
                { value: "all", label: "All interfaces", title: "0.0.0.0" },
                { value: "custom", label: "Custom", title: "A specific address" },
              ]}
            />
          </div>
          <div class="field">
            <label for="fw-bport">Port</label>
            <input
              id="fw-bport"
              class="input mono"
              bind:value={form.bindPort}
              placeholder={form.kind === "remote" ? "8080" : "auto"}
              inputmode="numeric"
              autocomplete="off"
              aria-invalid={!!show("bindPort")}
              aria-describedby="fw-bindPort-msg"
            />
          </div>
          {#if form.bindMode === "custom"}
            <div class="field span-full">
              <label for="fw-bcustom">Listen address</label>
              <input
                id="fw-bcustom"
                class="input mono"
                bind:value={form.bindCustom}
                placeholder="192.168.1.10"
                autocomplete="off"
                spellcheck="false"
                aria-invalid={!!show("bindCustom")}
                aria-describedby="fw-bindCustom-msg"
              />
              {@render fieldError("bindCustom")}
            </div>
          {/if}
          <div class="span-full msgs">
            {#if show("bindPort")}
              <p class="error" id="fw-bindPort-msg">{show("bindPort")}</p>
            {:else if portWarning}
              <p class="warn-text" id="fw-bindPort-msg">{portWarning}</p>
            {:else if autoPort}
              <p class="help" id="fw-bindPort-msg">Empty = any free port, picked when the forward starts.</p>
            {/if}
            {#if exposeWarning}
              <div class="callout warning" role="note">
                <TriangleAlert size={14} />
                <span>{exposeWarning}</span>
              </div>
            {/if}
          </div>
        </div>
      </fieldset>

      {#if form.kind !== "dynamic"}
        <fieldset class="group">
          <legend class="group-title">Connect to</legend>
          {#if form.kind === "remote"}
            <div class="field from">
              <span class="label" id="fw-from-label">Connect from</span>
              <div class="from-row">
                <Segmented
                  name="fw-from"
                  size="sm"
                  labelledby="fw-from-label"
                  bind:value={form.from}
                  options={[
                    { value: "here", label: "This computer" },
                    { value: "host", label: "Another host" },
                  ]}
                />
                {#if form.from === "host"}
                  {@render hostSelect("fw-via", form.viaHostId, (v) => (form.viaHostId = v), form.hostId || null, !!show("viaHostId"), "Choose a host…")}
                {/if}
              </div>
              {#if form.from === "host"}
                {@render fieldError("viaHostId", "Machine-to-machine: Patchbay relays the traffic between the two hosts.")}
              {/if}
            </div>
          {/if}
          <div class="presets" role="group" aria-label="Common services">
            {#each PRESETS as p (p.label)}
              <button type="button" class="chip" class:on={presetActive(p)} aria-pressed={presetActive(p)} onclick={() => applyPreset(p)}>
                {p.label}<span class="chip-port">{p.port}</span>
              </button>
            {/each}
          </div>
          <div class="grid">
            <div class="field span-2">
              <label for="fw-thost">Host</label>
              <input
                id="fw-thost"
                class="input mono"
                bind:value={form.targetHost}
                onblur={splitTarget}
                placeholder="localhost or db.internal"
                autocomplete="off"
                spellcheck="false"
                aria-invalid={!!show("targetHost")}
                aria-describedby="fw-targetHost-msg"
              />
            </div>
            <div class="field">
              <label for="fw-tport">Port</label>
              <input
                id="fw-tport"
                class="input mono"
                bind:value={form.targetPort}
                placeholder="5432"
                inputmode="numeric"
                autocomplete="off"
                aria-invalid={!!show("targetPort")}
                aria-describedby="fw-targetPort-msg"
              />
            </div>
            <div class="span-full msgs">
              {#if show("targetHost")}
                <p class="error" id="fw-targetHost-msg">{show("targetHost")}</p>
              {:else if show("targetPort")}
                <p class="error" id="fw-targetPort-msg">{show("targetPort")}</p>
              {:else}
                <p class="help" id="fw-targetHost-msg">
                  As seen from <strong>{seenFrom}</strong>{isLoopback(form.targetHost.trim() || null) ? ` — “localhost” is ${seenFrom} itself.` : "."}
                </p>
              {/if}
            </div>
          </div>
        </fieldset>
      {/if}

      <div class="options">
        <label class="checkbox">
          <input type="checkbox" bind:checked={form.autoStart} />
          <span>
            <span class="opt-label">Start automatically</span>
            <span class="help">When Patchbay launches. Reconnects on its own if the connection drops.</span>
          </span>
        </label>
        {#if running}
          <p class="help restart-note">This forward is running; saving restarts it with the new settings.</p>
        {:else}
          <label class="checkbox">
            <input type="checkbox" bind:checked={startAfter} />
            <span class="opt-label">Start after saving</span>
          </label>
        {/if}
      </div>
    </div>

    <div class="dialog-footer">
      {#if saveError}
        <p class="save-error" role="alert">{saveError}</p>
      {/if}
      <span class="spacer"></span>
      <button type="button" class="btn" onclick={close}>Cancel</button>
      <button type="submit" class="btn btn-primary" disabled={saving} title="{editor.mode === 'create' ? 'Add forward' : 'Save'} ({modKey}+Enter)">
        {editor.mode === "create" ? "Add forward" : "Save changes"}
      </button>
    </div>
  </form>
</Dialog>

<style>
  .fwd-editor {
    display: contents;
  }
  .dialog-body {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 5px;
    min-width: 0;
  }
  label,
  .label {
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
  }
  .grid {
    display: grid;
    grid-template-columns: 1fr 1fr 110px;
    gap: 10px 12px;
    align-items: end;
  }
  .span-2 {
    grid-column: span 2;
  }
  .span-full {
    grid-column: 1 / -1;
  }
  @media (max-width: 560px) {
    .grid {
      grid-template-columns: 1fr 88px;
    }
    .span-2 {
      grid-column: span 1;
    }
  }
  .group {
    min-width: 0;
    margin: 0;
    padding: 14px 0 0;
    border: 0;
    border-top: 1px solid var(--border-subtle);
  }
  .group-title {
    float: left;
    width: 100%;
    margin: 0 0 10px;
    padding: 0;
    color: var(--text-muted);
    font-size: var(--text-xs);
    font-weight: var(--weight-semibold);
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  .group-title + * {
    clear: both;
  }
  .msgs {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-top: -4px;
  }
  .msgs:empty {
    display: none;
  }
  .help,
  .error,
  .warn-text {
    font-size: var(--text-xs);
    line-height: 1.45;
  }
  .help {
    color: var(--text-subtle);
  }
  .help strong {
    color: var(--text-muted);
    font-weight: var(--weight-medium);
  }
  .error {
    color: var(--danger-text);
  }
  .warn-text {
    color: var(--warning-text);
  }
  .select {
    padding-right: 6px;
  }
  .callout {
    display: flex;
    gap: 8px;
    padding: 8px 10px;
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    line-height: 1.4;
  }
  .callout :global(svg) {
    flex: none;
    margin-top: 2px;
  }
  .callout.warning {
    background: var(--warning-soft);
    color: var(--warning-text);
  }
  .from {
    margin-bottom: 12px;
  }
  .from-row {
    display: grid;
    grid-template-columns: minmax(0, 240px) minmax(0, 1fr);
    gap: 8px;
    align-items: center;
  }
  @media (max-width: 560px) {
    .from-row {
      grid-template-columns: 1fr;
    }
  }
  .presets {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
    margin-bottom: 10px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 24px;
    padding: 0 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius-full);
    background: var(--bg-elevated);
    color: var(--text-muted);
    font-size: var(--text-sm);
    transition:
      background var(--dur-fast),
      border-color var(--dur-fast),
      color var(--dur-fast);
  }
  .chip:hover {
    border-color: var(--border-strong);
    color: var(--text);
  }
  .chip.on {
    border-color: color-mix(in srgb, var(--accent) 45%, transparent);
    background: var(--accent-soft);
    color: var(--accent-text);
  }
  .chip-port {
    color: var(--text-subtle);
    font-family: var(--font-mono);
    font-size: 10.5px;
  }
  .chip.on .chip-port {
    color: inherit;
    opacity: 0.8;
  }
  .options {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding-top: 14px;
    border-top: 1px solid var(--border-subtle);
  }
  .options .checkbox {
    align-items: flex-start;
    color: var(--text);
  }
  .options .checkbox input {
    margin-top: 2px;
  }
  .options .checkbox > span {
    display: flex;
    flex-direction: column;
  }
  .opt-label {
    font-weight: var(--weight-medium);
  }
  .restart-note {
    font-size: var(--text-sm);
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
</style>
