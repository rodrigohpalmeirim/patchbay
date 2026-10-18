<!--
  Small left-to-right picture of a forward's route: the listening side on the
  left, SSH tunnels as tubes, the last plain TCP hop as a dashed line.
    local    this computer ⇄ SSH host → target
    remote   SSH host ⇄ this computer → target   (or ⇄ via host → target)
    dynamic  this computer ⇄ SSH host → anywhere
-->
<script lang="ts">
  import type { LucideIcon } from "@lucide/svelte";
  import Crosshair from "@lucide/svelte/icons/crosshair";
  import Globe from "@lucide/svelte/icons/globe";
  import Laptop from "@lucide/svelte/icons/laptop";
  import Server from "@lucide/svelte/icons/server";
  import type { PortForwardKind } from "../types";

  let {
    kind,
    host,
    via = null,
    listen,
    target = null,
    targetLoopback = false,
    exposed = false,
    active = false,
  }: {
    kind: PortForwardKind;
    /** Name of the carrying SSH host. */
    host: string;
    /** Remote only: host the target is reached from (machine-to-machine). */
    via?: string | null;
    /** Listening port label, e.g. ":5432" or "auto". */
    listen: string;
    /** "db.internal:5432"; ignored for dynamic. */
    target?: string | null;
    /** Target is localhost of the machine it's reached from. */
    targetLoopback?: boolean;
    /** Listener accepts connections from other machines (shown as 0.0.0.0). */
    exposed?: boolean;
    /** Animate the tunnels (the forward is running). */
    active?: boolean;
  } = $props();

  interface Node {
    icon: LucideIcon;
    title: string;
    detail: string | null;
    listen?: boolean;
    end?: boolean;
  }
  type Link = "ssh" | "tcp";

  const THIS = "This computer";

  const model = $derived.by((): { nodes: Node[]; links: Link[] } => {
    const listenDetail = exposed && listen.startsWith(":") ? `0.0.0.0${listen}` : listen;
    const tgt = target || "target";
    // Split "host:port" for a two-line target node.
    const cut = tgt.lastIndexOf(":");
    const targetNode: Node = {
      icon: Crosshair,
      title: cut > 0 ? tgt.slice(0, cut) : tgt,
      detail: cut > 0 ? tgt.slice(cut) : null,
      end: true,
    };
    switch (kind) {
      case "local":
        return targetLoopback
          ? {
              nodes: [
                { icon: Laptop, title: THIS, detail: listenDetail, listen: true },
                { icon: Server, title: host, detail: tgt, end: true },
              ],
              links: ["ssh"],
            }
          : {
              nodes: [{ icon: Laptop, title: THIS, detail: listenDetail, listen: true }, { icon: Server, title: host, detail: null }, targetNode],
              links: ["ssh", "tcp"],
            };
      case "dynamic":
        return {
          nodes: [
            { icon: Laptop, title: THIS, detail: `SOCKS ${listenDetail}`, listen: true },
            { icon: Server, title: host, detail: null },
            { icon: Globe, title: "Anywhere", detail: "per request", end: true },
          ],
          links: ["ssh", "tcp"],
        };
      case "remote": {
        const head: Node = { icon: Server, title: host, detail: listenDetail, listen: true };
        if (via) {
          return targetLoopback
            ? {
                nodes: [head, { icon: Laptop, title: THIS, detail: "relays" }, { icon: Server, title: via, detail: tgt, end: true }],
                links: ["ssh", "ssh"],
              }
            : {
                nodes: [head, { icon: Laptop, title: THIS, detail: "relays" }, { icon: Server, title: via, detail: null }, targetNode],
                links: ["ssh", "ssh", "tcp"],
              };
        }
        return targetLoopback
          ? { nodes: [head, { icon: Laptop, title: THIS, detail: tgt, end: true }], links: ["ssh"] }
          : { nodes: [head, { icon: Laptop, title: THIS, detail: null }, targetNode], links: ["ssh", "tcp"] };
      }
    }
  });

  const summary = $derived(
    model.nodes.map((n) => `${n.title}${n.detail ? ` (${n.detail})` : ""}`).join(model.links.length ? " → " : "") +
      (model.links.includes("ssh") ? ", tunnelled over SSH" : ""),
  );
</script>

<div class="diagram" class:active role="img" aria-label="Route: {summary}">
  {#each model.nodes as n, i (i)}
    {#if i > 0}
      {@const link = model.links[i - 1]}
      <div class="link {link}" aria-hidden="true">
        <span class="link-label">{link === "ssh" ? "SSH" : "TCP"}</span>
        <span class="line"></span>
      </div>
    {/if}
    <div class="node" class:listen={n.listen} class:end={n.end} aria-hidden="true">
      <span class="node-icon"><n.icon size={16} /></span>
      <span class="node-title" title={n.title}>{n.title}</span>
      {#if n.detail}<span class="node-detail mono" title={n.detail}>{n.detail}</span>{/if}
    </div>
  {/each}
</div>

<style>
  .diagram {
    display: flex;
    align-items: flex-start;
    justify-content: center;
    gap: 4px;
    min-width: 0;
    padding: 14px 12px 12px;
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
    flex: 0 1 116px;
    min-width: 56px;
    text-align: center;
  }
  .node-icon {
    position: relative;
    display: grid;
    place-items: center;
    width: 36px;
    height: 36px;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    color: var(--text-muted);
    box-shadow: var(--shadow-sm);
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
  .end .node-icon {
    border-style: dashed;
  }
  .node-title {
    max-width: 100%;
    margin-top: 6px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: var(--text-sm);
    font-weight: var(--weight-medium);
    line-height: 1.3;
  }
  .node-detail {
    max-width: 100%;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--text-muted);
    font-size: 11px;
    line-height: 1.35;
  }
  .listen .node-detail {
    color: var(--accent-text);
  }

  .link {
    position: relative;
    display: flex;
    align-items: center;
    flex: 1 1 48px;
    min-width: 26px;
    max-width: 120px;
    height: 36px;
  }
  .link-label {
    position: absolute;
    left: 50%;
    top: 0;
    transform: translateX(-50%);
    color: var(--text-subtle);
    font-size: 9.5px;
    font-weight: var(--weight-semibold);
    letter-spacing: 0.06em;
  }
  .ssh .link-label {
    color: var(--accent-text);
  }
  .line {
    position: relative;
    flex: 1;
  }
  /* Arrow head: connections travel left to right. */
  .line::after {
    content: "";
    position: absolute;
    right: -1px;
    top: 50%;
    width: 0;
    height: 0;
    border-left: 6px solid currentColor;
    border-top: 4px solid transparent;
    border-bottom: 4px solid transparent;
    transform: translateY(-50%);
  }
  .ssh .line {
    height: 6px;
    margin-right: 2px;
    border-radius: 3px;
    background:
      linear-gradient(90deg, var(--accent) 0 6px, transparent 6px) 0 50% / 10px 2px repeat-x,
      var(--accent-soft);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 35%, transparent);
    color: var(--accent);
  }
  .ssh .line::after {
    right: -7px;
  }
  .active .ssh .line {
    animation: flow 1.2s linear infinite;
  }
  .tcp .line {
    height: 0;
    margin-right: 6px;
    border-top: 1.5px dashed var(--text-subtle);
    color: var(--text-subtle);
  }
  .tcp .line::after {
    right: -7px;
    top: -0.75px;
  }
  @keyframes flow {
    from {
      background-position:
        0 50%,
        0 0;
    }
    to {
      background-position:
        10px 50%,
        0 0;
    }
  }
</style>
