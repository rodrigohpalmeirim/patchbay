<!--
  Create / edit a port forward rule. The form is the route itself, read left to
  right: the machine where the port opens and the machine connections go out
  from (the SSH tunnel runs between the two), then either one service or, as a
  SOCKS proxy, anywhere. The kind follows from that:
                     service    anywhere (SOCKS)
    open here        local      dynamic
    open on server   remote     remoteDynamic
  Opening on a server and going out from another server is relayed by Patchbay.
-->
<script lang="ts">
  import ArrowLeftRight from "@lucide/svelte/icons/arrow-left-right";
  import Cable from "@lucide/svelte/icons/cable";
  import Globe from "@lucide/svelte/icons/globe";
  import Plug from "@lucide/svelte/icons/plug";
  import Laptop from "@lucide/svelte/icons/laptop";
  import Server from "@lucide/svelte/icons/server";
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";
  import { tick, untrack } from "svelte";
  import Dialog from "../components/Dialog.svelte";
  import Segmented from "../components/Segmented.svelte";
  import { hostPort, isAllInterfaces, isLoopback, isSocks, KIND_FLAG, listensRemotely } from "../forwards/ops";
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
    /** Remote only: "" = this computer connects to the target, else another host does. */
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
      targetHost: src.targetHost ?? "localhost",
      targetPort: src.targetPort ? String(src.targetPort) : "",
      viaHostId: src.viaHostId ?? "",
      autoStart: src.autoStart ?? false,
    };
  }

  const initial = untrack(initialForm);
  let form = $state<Form>(structuredClone(initial));
  let submitted = $state(false);
  let saving = $state(false);
  let saveError = $state<string | null>(null);

  /** Field focused when the dialog opens: the first thing still missing. */
  const autofocus = untrack(() => (!form.hostId ? "host" : isSocks(form.kind) ? "bport" : "tport"));

  // ------------------------------------------------------------- route

  /** Machine select value for this computer ("" = nothing chosen yet). */
  const HERE = "here";
  const remoteListen = $derived(listensRemotely(form.kind));
  const socks = $derived(isSocks(form.kind));
  /** Where the port opens. */
  const openOn = $derived(remoteListen ? form.hostId : HERE);
  /** The machine connections go out from. */
  const reachedFrom = $derived(remoteListen ? form.viaHostId || HERE : form.hostId);

  function setRoute(open: string, from: string, anywhere = socks) {
    if (open === HERE) {
      form.kind = anywhere ? "dynamic" : "local";
      form.hostId = from === HERE ? "" : from;
      form.viaHostId = "";
    } else {
      form.kind = anywhere ? "remoteDynamic" : "remote";
      form.hostId = open;
      form.viaHostId = from === HERE ? "" : from;
    }
    // A proxy has no service port to borrow; offer the usual SOCKS port, and take it back
    // when going back to a service.
    const bp = form.bindPort.trim();
    if (anywhere && (!bp || bp === presetBindPort)) form.bindPort = socksPort = String(freePort(1080));
    else if (!anywhere && socksPort && bp === socksPort) form.bindPort = socksPort = "";
  }
  let socksPort = "";
  // Picking the machine the other end already uses swaps the two ends.
  const setOpenOn = (v: string) => setRoute(v, v === reachedFrom ? openOn : reachedFrom);
  const setReachedFrom = (v: string) => setRoute(v === openOn ? reachedFrom : openOn, v);
  const swap = () => setRoute(reachedFrom || HERE, openOn);
  const setDestination = (v: "service" | "anywhere") => setRoute(openOn, reachedFrom, v === "anywhere");

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
  const via = $derived(remoteListen && form.viaHostId ? hosts.get(form.viaHostId) : undefined);
  const machineName = (id: string) => (id === HERE ? "this computer" : (hosts.get(id)?.name ?? "the server"));
  const hostLabel = $derived(host?.name ?? "the server");

  // ------------------------------------------------------------ presets

  interface Preset {
    label: string;
    port: number;
    /** Suggested listening port when the service port is privileged. */
    listen?: number;
  }
  /** Services usually reached on a server, and on this computer (shared with a server). */
  const PRESETS: Record<"server" | "here", Preset[]> = {
    server: [
      { label: "Dev server", port: 3000 },
      { label: "Vite", port: 5173 },
      { label: "Web", port: 80, listen: 8080 },
      { label: "Web", port: 8080 },
      { label: "PostgreSQL", port: 5432 },
      { label: "MySQL", port: 3306 },
      { label: "Redis", port: 6379 },
      { label: "MongoDB", port: 27017 },
    ],
    here: [
      { label: "Dev server", port: 3000 },
      { label: "Vite", port: 5173 },
      { label: "Web", port: 8080 },
      { label: "Web", port: 8000 },
      { label: "Web", port: 80, listen: 8080 },
      { label: "Jupyter", port: 8888 },
      { label: "PostgreSQL", port: 5432 },
      { label: "Redis", port: 6379 },
    ],
  };
  const allPresets = $derived(PRESETS[reachedFrom === HERE ? "here" : "server"]);
  const presets = $derived(socks ? null : allPresets);
  const chipLabel = (p: Preset) => (p.label === "Web" ? `Web ${p.port}` : p.label);
  const activePreset = $derived(socks ? undefined : allPresets.find((p) => form.targetPort.trim() === String(p.port)));

  /** Ports other rules already listen on, on the same side as this one. */
  const takenPorts = $derived(
    new Set(
      forwards.list
        .filter((f) => f.id !== selfId && listensRemotely(f.kind) === remoteListen && (!remoteListen || f.hostId === form.hostId))
        .map((f) => f.bindPort),
    ),
  );

  function freePort(from: number): number {
    let port = from;
    while (takenPorts.has(port) && port < 65535) port++;
    return port;
  }

  /** The listening port the last preset filled in, which the next one may replace. */
  let presetBindPort: string | null = null;

  function applyPreset(p: Preset) {
    const bp = form.bindPort.trim();
    const fromPreset = !bp || bp === presetBindPort || allPresets.some((x) => bp === String(x.listen ?? x.port));
    if (fromPreset) {
      form.bindPort = presetBindPort = String(freePort(p.listen ?? (p.port < 1024 ? 8000 + p.port : p.port)));
    }
    form.targetPort = String(p.port);
    if (!form.targetHost.trim()) form.targetHost = "localhost";
  }

  // --------------------------------------------------------- validation

  const portOf = (v: string) => (/^\d+$/.test(v.trim()) ? Number(v.trim()) : NaN);

  /** The port to open; on a server it defaults to the service's port. */
  const bindPort = $derived(form.bindPort.trim() || (form.kind === "remote" ? form.targetPort.trim() : ""));

  const bindAddress = $derived(
    form.bindMode === "loopback" ? null : form.bindMode === "all" ? "0.0.0.0" : form.bindCustom.trim() || null,
  );
  const bindShown = $derived(form.bindMode === "loopback" ? "localhost" : form.bindMode === "all" ? "0.0.0.0" : form.bindCustom.trim() || "address");
  const exposed = $derived(form.bindMode === "all" || (form.bindMode === "custom" && !isLoopback(form.bindCustom.trim() || null)));
  const autoPort = $derived(!remoteListen && (bindPort === "" || bindPort === "0"));
  const targetLoopback = $derived(isLoopback(form.targetHost.trim() || null));

  const suggestedName = $derived.by(() => {
    const on = host ? ` on ${host.name}` : "";
    if (form.kind === "dynamic") return host ? `SOCKS proxy via ${host.name}` : "SOCKS proxy";
    if (form.kind === "remoteDynamic") return host ? `SOCKS proxy on ${host.name}${via ? ` via ${via.name}` : ""}` : "SOCKS proxy";
    const tp = form.targetPort.trim();
    if (!host && !tp) return "";
    const what = activePreset ? chipLabel(activePreset) : tp ? `Port ${tp}` : "Forward";
    if (form.kind === "remote") return host ? `${what} for ${host.name}` : what;
    const th = form.targetHost.trim();
    return activePreset || targetLoopback || !th ? `${what}${on}` : `${hostPort(th, tp || "port")}${on}`;
  });
  const effectiveName = $derived(form.name.trim() || forwards.uniqueName(suggestedName));

  const errors = $derived.by(() => {
    const e: Partial<Record<keyof Form, string>> = {};
    const clash = forwards.list.find((f) => f.id !== selfId && f.name.toLowerCase() === effectiveName.toLowerCase());
    if (clash) e.name = "Another forward already has this name.";
    if (!form.hostId) e.hostId = "Choose the server at the other end of the route.";
    else if (!host) e.hostId = "This host no longer exists.";
    if (form.bindMode === "custom") {
      const a = form.bindCustom.trim();
      if (!a) e.bindCustom = "Enter the address to listen on, e.g. 192.168.1.10.";
      else if (/\s|\/|^\[?[^\]]*\]?:\d+$/.test(a) && !a.includes("::")) e.bindCustom = "Enter just the address to listen on, without a port.";
    }
    const bp = bindPort;
    if (remoteListen) {
      if (!bp) e.bindPort = `Choose the port to open on ${hostLabel}.`;
      else if (!(portOf(bp) >= 1 && portOf(bp) <= 65535)) e.bindPort = "Use a port between 1 and 65535.";
    } else if (bp && !(portOf(bp) >= 0 && portOf(bp) <= 65535)) {
      e.bindPort = "Use a port between 1 and 65535, or leave it empty for any free port.";
    }
    if (!socks) {
      const th = form.targetHost.trim();
      if (!th) e.targetHost = "Enter the service's host, e.g. localhost.";
      else if (/^[a-z][a-z0-9+.-]*:\/\//i.test(th)) e.targetHost = "Enter just the service's host name, without http:// or a path.";
      else if (/[\s/]/.test(th)) e.targetHost = "The service's host name can't contain spaces or slashes.";
      const tp = form.targetPort.trim();
      if (!tp) e.targetPort = "Enter the service's port, or pick one of the common services.";
      else if (!(portOf(tp) >= 1 && portOf(tp) <= 65535)) e.targetPort = "Use a service port between 1 and 65535.";
    }
    if (remoteListen && form.viaHostId && !hosts.get(form.viaHostId)) e.viaHostId = "The host connections go out from no longer exists.";
    return e;
  });
  const valid = $derived(Object.keys(errors).length === 0);
  const show = (field: keyof Form) => (submitted ? errors[field] : undefined);
  /** Route errors, in the order the fields appear. */
  const routeErrors = $derived(
    submitted
      ? (["hostId", "bindCustom", "bindPort", "viaHostId", "targetHost", "targetPort"] as const).flatMap((f) => (errors[f] ? [errors[f]] : []))
      : [],
  );

  /** Soft warnings that don't block saving. */
  const portWarning = $derived.by(() => {
    const p = portOf(bindPort);
    if (!Number.isFinite(p) || p === 0) return null;
    if (remoteListen) return p < 1024 ? `Ports below 1024 usually need root on ${hostLabel}.` : null;
    if (p < 1024) return "Ports below 1024 need administrator rights on most systems.";
    const clash = forwards.list.find((f) => f.id !== selfId && f.kind !== "remote" && f.bindPort === p);
    return clash ? `“${clash.name}” also listens on ${p}; only one of them can run at a time.` : null;
  });

  const exposeWarning = $derived.by(() => {
    if (form.kind === "remoteDynamic") {
      // Even on loopback, other users of the server can use it.
      const who = exposed ? `Anyone who can reach ${hostLabel}` : `Anyone who can log in to ${hostLabel}`;
      const what = reachedFrom === HERE ? "anything this computer can, including your local network" : `anything ${fromName} can`;
      return `${who} can use this proxy to reach ${what}.${exposed ? " The server must allow it (GatewayPorts in sshd_config)." : ""}`;
    }
    if (!exposed) return null;
    if (form.kind === "remote")
      return `Anyone who can reach ${hostLabel} can connect to this port. The server must allow it (GatewayPorts in sshd_config).`;
    if (form.kind === "dynamic") return "Other devices on your network can use this proxy to reach anything the server can.";
    return "Other devices on your network can connect to this port.";
  });

  // Paste "host:port" into the service host: split it.
  function splitTarget() {
    const v = form.targetHost.trim();
    const m = /^\[?([^\]]+?)\]?:(\d{1,5})$/.exec(v);
    if (m && !v.includes("::") && !form.targetPort.trim()) {
      form.targetHost = m[1];
      form.targetPort = m[2];
    }
  }

  async function onbindmode(mode: BindMode) {
    form.bindMode = mode;
    if (mode === "custom") {
      await tick();
      document.getElementById("fw-bcustom")?.focus();
    }
  }

  // ---------------------------------------------------------------- copy

  const KIND_INFO: Record<PortForwardKind, string> = {
    local: "Use a service on or behind a server as if it ran on this computer.",
    remote: "Let a server reach a service on this computer or on another server.",
    dynamic: "A SOCKS5 proxy on this computer. Apps that use it go online from the server.",
    remoteDynamic: "A SOCKS5 proxy on a server. Apps there that use it go online from elsewhere.",
  };

  const SERVICE_TITLE = `One service, e.g. a database (ssh ${KIND_FLAG.local} / ${KIND_FLAG.remote})`;
  const SOCKS_TITLE = `Anywhere: apps pick the destination (ssh ${KIND_FLAG.dynamic} / ${KIND_FLAG.remote} port)`;

  const bindOptions = $derived<{ value: BindMode; label: string }[]>(
    remoteListen
      ? [
          { value: "loopback", label: `Only ${hostLabel} itself` },
          { value: "all", label: `Anyone who can reach ${hostLabel}` },
          { value: "custom", label: "A specific address…" },
        ]
      : [
          { value: "loopback", label: "Only this computer" },
          { value: "all", label: "Devices on your network" },
          { value: "custom", label: "A specific address…" },
        ],
  );

  const listenPort = $derived(autoPort ? null : bindPort || "port");
  const listenAddr = $derived(listenPort ? hostPort(bindShown, listenPort) : null);
  const targetAddr = $derived(hostPort(form.targetHost.trim() || "host", form.targetPort.trim() || "port"));
  const fromName = $derived(machineName(reachedFrom || "")); 

  // ---------------------------------------------------------------- save

  function toDraft(): PortForwardDraft {
    const p = portOf(bindPort);
    return {
      name: effectiveName,
      kind: form.kind,
      hostId: form.hostId as HostId,
      bindAddress,
      bindPort: Number.isFinite(p) ? p : 0,
      targetHost: socks ? null : form.targetHost.trim(),
      targetPort: socks ? null : portOf(form.targetPort),
      viaHostId: remoteListen && form.viaHostId ? (form.viaHostId as HostId) : null,
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
      } else {
        const f = await forwards.create(draft);
        toasts.success(`Added “${f.name}”`);
        ui.forwardEditor = null;
        void forwards.start(f.id);
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
  const submitLabel = $derived(editor.mode === "create" ? "Add and start" : running ? "Save and restart" : "Save");
</script>

{#snippet hostOptions(exclude: string | null)}
  {#each hostGroups as g (g.label)}
    <optgroup label={g.label}>
      {#each g.hosts.filter((h) => h.id !== exclude) as h (h.id)}
        <option value={h.id}>{h.name}</option>
      {/each}
    </optgroup>
  {/each}
{/snippet}

{#snippet machineSelect(id: string, value: string, onchange: (v: string) => void, label: string, invalid: boolean)}
  <select
    {id}
    class="input select"
    {value}
    onchange={(e) => onchange(e.currentTarget.value)}
    aria-label={label}
    aria-invalid={invalid}
    title={hosts.get(value) ? address(hosts.get(value)!) : undefined}
    data-autofocus={(id === "fw-from" && autofocus === "host") || undefined}
  >
    {#if !value}<option value="" disabled>Choose a server…</option>{/if}
    <option value={HERE}>This computer</option>
    {@render hostOptions(null)}
  </select>
{/snippet}

{#snippet listenField(label: string)}
  <div class="hostport">
    {#if form.bindMode === "custom"}
      <input
        id="fw-bcustom"
        class="input mono"
        bind:value={form.bindCustom}
        placeholder="192.168.1.10"
        autocomplete="off"
        spellcheck="false"
        aria-label="{label}: address"
        aria-invalid={!!show("bindCustom")}
      />
    {:else}
      <input class="input mono" value={bindShown} readonly tabindex="-1" aria-label="{label}: address" title="Change this with “Who can connect”" />
    {/if}
    <span class="colon">:</span>
    <input
      id="fw-bport"
      class="input mono port"
      bind:value={form.bindPort}
      placeholder={remoteListen ? (socks ? "1080" : form.targetPort.trim() || "8080") : "auto"}
      inputmode="numeric"
      autocomplete="off"
      aria-label="{label}: port"
      aria-invalid={!!show("bindPort")}
      title={remoteListen ? (socks ? undefined : "Leave empty to use the service's port") : "Leave empty for any free port, picked when the forward starts"}
      data-autofocus={autofocus === "bport" || undefined}
    />
  </div>
{/snippet}

{#snippet targetField()}
  <div class="hostport">
    <input
      id="fw-thost"
      class="input mono"
      bind:value={form.targetHost}
      onblur={splitTarget}
      placeholder="localhost"
      autocomplete="off"
      spellcheck="false"
      aria-label="Service host, as seen from {fromName}"
      aria-invalid={!!show("targetHost")}
    />
    <span class="colon">:</span>
    <input
      id="fw-tport"
      class="input mono port"
      bind:value={form.targetPort}
      placeholder="5432"
      inputmode="numeric"
      autocomplete="off"
      aria-label="Service port"
      aria-invalid={!!show("targetPort")}
      data-autofocus={autofocus === "tport" || undefined}
    />
  </div>
{/snippet}

<!-- The service in the summary sentence; `on` is the machine it is reached from. -->
{#snippet targetDesc(on: string)}
  {@const th = form.targetHost.trim()}
  {@const tp = form.targetPort.trim()}
  {#if targetLoopback}
    {#if tp}port <code>{tp}</code> on {on === "this computer" ? on : `${on} itself`}{:else}a service on {on === "this computer" ? on : `${on} itself`}{/if}
  {:else if tp}<code>{targetAddr}</code>{:else}<code>{th}</code>{/if}
{/snippet}

{#snippet link()}
  <div class="link" aria-hidden="true">
    <span class="link-label">SSH</span>
    <span class="line"></span>
  </div>
{/snippet}

<Dialog onclose={close} labelledby="fw-title" width={680} dismissable={false}>
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
      <Segmented
          name="fw-dest"
          label="What to forward"
          value={socks ? "anywhere" : "service"}
          onchange={setDestination}
          options={[
            { value: "service", label: "One service", icon: Plug, title: SERVICE_TITLE },
            { value: "anywhere", label: "Anywhere (SOCKS proxy)", icon: Globe, title: SOCKS_TITLE },
          ]}
        />
      <div class="route" role="group" aria-label="Route">
        <div class="node listen">
          <span class="node-icon">{#if openOn === HERE}<Laptop size={18} />{:else}<Server size={18} />{/if}</span>
          <span class="node-title">Open on</span>
          {@render machineSelect("fw-open", openOn, setOpenOn, "Open the port on", remoteListen && !!show("hostId"))}
          {@render listenField("Open on")}
        </div>
        <div class="mid">
          <div class="links">
            {@render link()}
            {#if via}
              <div class="relay" title="Patchbay carries the traffic between the two servers">
                <span class="node-icon sm"><Laptop size={14} /></span>
                <span class="relay-label">Relayed by this computer</span>
              </div>
              {@render link()}
            {/if}
          </div>
          <button type="button" class="icon-btn sm swap" onclick={swap} disabled={!reachedFrom} title="Swap the two ends" aria-label="Swap the two ends">
            <ArrowLeftRight size={13} />
          </button>
        </div>
        <div class="node end">
          <span class="node-icon">{#if socks}<Globe size={18} />{:else if reachedFrom === HERE}<Laptop size={18} />{:else}<Server size={18} />{/if}</span>
          <span class="node-title">{socks ? "Goes online from" : "Service from"}</span>
          {@render machineSelect("fw-from", reachedFrom, setReachedFrom, socks ? "Go online from" : "Reach the service from", !remoteListen && !!show("hostId"))}
          {#if socks}
            <span class="node-hint">Apps pick where to connect</span>
          {:else}
            {@render targetField()}
            <span class="node-hint">{targetLoopback ? `localhost is ${fromName} itself` : `As seen from ${fromName}`}</span>
          {/if}
        </div>
      </div>

      {#if presets}
        <div class="presets" role="group" aria-label="Common services">
          {#each presets as p (p.port)}
            <button type="button" class="chip" class:on={activePreset === p} aria-pressed={activePreset === p} onclick={() => applyPreset(p)} title="Port {p.port}">
              {chipLabel(p)}
            </button>
          {/each}
        </div>
      {/if}

      <p class="sentence" aria-live="polite">
        {#if form.kind === "local"}
          Apps on this computer{exposed ? " and your network" : ""} connect to
          {#if listenAddr}<code>{listenAddr}</code>{:else}a free port on <code>{bindShown}</code>{/if}
          and reach {@render targetDesc(hostLabel)}{targetLoopback ? "" : ` through ${hostLabel}`}.
        {:else if form.kind === "remote"}
          Apps on {hostLabel}{exposed ? " and its network" : ""} connect to
          {#if bindPort}<code>{listenAddr}</code>{:else}the port you choose{/if}
          and reach {@render targetDesc(fromName)}{!targetLoopback && form.viaHostId ? ` as seen from ${fromName}` : ""}{form.viaHostId ? ", relayed by Patchbay" : ""}.
        {:else if form.kind === "dynamic"}
          Apps set to use the SOCKS5 proxy
          {#if listenAddr}<code>{listenAddr}</code>{:else}on a free port of <code>{bindShown}</code>{/if}
          go online from {hostLabel}.
        {:else}
          Apps on {hostLabel} set to use the SOCKS5 proxy
          {#if bindPort}<code>{listenAddr}</code>{:else}on the port you choose{/if}
          go online from {fromName}{form.viaHostId ? ", relayed by Patchbay" : ""}.
        {/if}
      </p>

      {#if routeErrors.length || portWarning || exposeWarning || (autoPort && form.kind === "local")}
        <div class="msgs">
          {#each routeErrors as err (err)}
            <p class="error">{err}</p>
          {/each}
          {#if portWarning}
            <p class="warn-text">{portWarning}</p>
          {:else if autoPort && !routeErrors.length}
            <p class="help">The port is picked when the forward starts. Enter one to keep it the same every time.</p>
          {/if}
          {#if exposeWarning}
            <div class="callout warning" role="note">
              <TriangleAlert size={14} />
              <span>{exposeWarning}</span>
            </div>
          {/if}
        </div>
      {/if}

      <div class="row2">
        <div class="field">
          <label for="fw-name">Name</label>
          <input
            id="fw-name"
            class="input"
            bind:value={form.name}
            placeholder={suggestedName || "Named after the service"}
            autocomplete="off"
            spellcheck="false"
            aria-invalid={!!show("name")}
            aria-describedby="fw-name-msg"
          />
          {#if show("name")}<p class="error" id="fw-name-msg">{show("name")}</p>{/if}
        </div>
        <div class="field">
          <label for="fw-bind">Who can connect</label>
          <select id="fw-bind" class="input select" value={form.bindMode} onchange={(e) => onbindmode(e.currentTarget.value as BindMode)}>
            {#each bindOptions as o (o.value)}
              <option value={o.value}>{o.label}</option>
            {/each}
          </select>
        </div>
      </div>
    </div>

    <div class="dialog-footer">
      {#if saveError}
        <p class="save-error" role="alert">{saveError}</p>
      {:else}
        <label class="checkbox" title="Reconnects on its own if the connection drops.">
          <input type="checkbox" bind:checked={form.autoStart} />
          <span>Start when Patchbay launches</span>
        </label>
      {/if}
      <span class="spacer"></span>
      <button type="button" class="btn" onclick={close}>Cancel</button>
      <button type="submit" class="btn btn-primary" disabled={saving} title="{submitLabel} ({modKey}+Enter)">{submitLabel}</button>
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
  label {
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
  }
  .row2 {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
    padding-top: 14px;
    border-top: 1px solid var(--border-subtle);
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
  .error {
    color: var(--danger-text);
  }
  .warn-text {
    color: var(--warning-text);
  }
  .select {
    padding-right: 6px;
  }
  .msgs {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: -6px;
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

  /* ----------------------------------------------------------- route */
  .route {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto minmax(0, 1fr);
    align-items: start;
    gap: 6px;
    padding: 16px 14px 14px;
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    background:
      radial-gradient(circle at 1px 1px, var(--border) 1px, transparent 0) 0 0 / 14px 14px,
      var(--bg-subtle);
  }
  .node {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 5px;
    min-width: 0;
    text-align: center;
  }
  .node > .select,
  .node > :global(.segmented) {
    width: 100%;
  }
  .node-icon {
    position: relative;
    display: grid;
    place-items: center;
    width: 36px;
    height: 36px;
    margin-bottom: 1px;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    color: var(--text-muted);
    box-shadow: var(--shadow-sm);
  }
  .node-icon.sm {
    width: 28px;
    height: 28px;
    margin: 4px 0 0;
    border-style: dashed;
  }
  .listen .node-icon {
    border-color: var(--accent);
    color: var(--accent-text);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  /* "Ear" on the listening machine: a dot where connections come in. */
  .listen .node-icon::before {
    content: "";
    position: absolute;
    left: -5px;
    top: 50%;
    width: 8px;
    height: 8px;
    margin-top: -4px;
    border: 2px solid var(--bg-subtle);
    border-radius: 50%;
    background: var(--accent);
  }
  .node-title {
    font-weight: var(--weight-semibold);
  }
  .node-hint {
    max-width: 100%;
    overflow: hidden;
    color: var(--text-subtle);
    font-size: var(--text-xs);
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .hostport {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto 62px;
    gap: 3px;
    align-items: center;
    width: 100%;
  }
  .colon {
    color: var(--text-subtle);
    font-family: var(--font-mono);
  }

  /* Between the machines: the SSH tunnel, through this computer for a relay. */
  .mid {
    position: relative;
    display: flex;
    flex-direction: column;
    align-items: center;
    width: 172px;
  }
  .links {
    display: flex;
    align-items: flex-start;
    width: 100%;
  }
  .relay {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 3px;
    flex: none;
    width: 76px;
  }
  .relay-label {
    color: var(--text-subtle);
    font-size: 10.5px;
    line-height: 1.25;
    text-align: center;
  }
  /* Level with the machine pickers it swaps. */
  .swap {
    position: absolute;
    top: 71px;
    left: 50%;
    transform: translateX(-50%);
    color: var(--text-subtle);
  }
  .link {
    position: relative;
    display: flex;
    flex: 1;
    align-items: center;
    height: 36px;
    min-width: 40px;
    padding: 0 4px;
  }
  .link-label {
    position: absolute;
    left: 50%;
    top: 0;
    transform: translateX(-50%);
    color: var(--accent-text);
    font-size: 9.5px;
    font-weight: var(--weight-semibold);
    letter-spacing: 0.06em;
  }
  .line {
    position: relative;
    flex: 1;
    height: 6px;
    margin-right: 6px;
    border-radius: 3px;
    background:
      linear-gradient(90deg, var(--accent) 0 6px, transparent 6px) 0 50% / 10px 2px repeat-x,
      var(--accent-soft);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 35%, transparent);
    color: var(--accent);
  }
  /* Arrow head: connections travel left to right. */
  .line::after {
    content: "";
    position: absolute;
    top: 50%;
    right: -7px;
    width: 0;
    height: 0;
    border-left: 6px solid currentColor;
    border-top: 4px solid transparent;
    border-bottom: 4px solid transparent;
    transform: translateY(-50%);
  }

  /* --------------------------------------------------------- presets */
  .presets {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    align-items: center;
    gap: 5px;
    margin-top: -4px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    height: 22px;
    padding: 0 9px;
    border: 1px solid var(--border);
    border-radius: var(--radius-full);
    background: var(--bg-elevated);
    color: var(--text-muted);
    font-size: var(--text-xs);
    line-height: 1;
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
  .sentence {
    color: var(--text-muted);
    font-size: var(--text-sm);
    line-height: 1.5;
    text-align: center;
    text-wrap: balance;
  }
  .sentence code {
    color: var(--text);
    font-family: var(--font-mono);
    font-size: 11.5px;
  }

  .checkbox {
    color: var(--text);
    font-weight: normal;
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
