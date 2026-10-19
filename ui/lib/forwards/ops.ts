// User-level port forward actions and display helpers shared by the forwards
// views, the overview and context menus.

import Copy from "@lucide/svelte/icons/copy";
import CopyPlus from "@lucide/svelte/icons/copy-plus";
import ExternalLink from "@lucide/svelte/icons/external-link";
import Pencil from "@lucide/svelte/icons/pencil";
import Play from "@lucide/svelte/icons/play";
import Server from "@lucide/svelte/icons/server";
import Square from "@lucide/svelte/icons/square";
import Trash2 from "@lucide/svelte/icons/trash-2";
import { forwards, isRunning } from "../stores/forwards.svelte";
import { hosts } from "../stores/hosts.svelte";
import { tabs } from "../stores/tabs.svelte";
import { toasts } from "../stores/toasts.svelte";
import { ui, type MenuItem } from "../stores/ui.svelte";
import type { HostId, PortForward, PortForwardDraft, PortForwardKind, PortForwardStatus } from "../types";
import { writeClipboard } from "../util/clipboard";
import { openInBrowser } from "../util/links";

export const KIND_LABEL: Record<PortForwardKind, string> = {
  local: "Local",
  remote: "Remote",
  dynamic: "SOCKS proxy",
  remoteDynamic: "Remote SOCKS proxy",
};

/** ssh-style flag, shown as a hint. */
export const KIND_FLAG: Record<PortForwardKind, string> = { local: "-L", remote: "-R", dynamic: "-D", remoteDynamic: "-R" };

/** Listens on the server (`hostId`) rather than on this computer. */
export const listensRemotely = (kind: PortForwardKind) => kind === "remote" || kind === "remoteDynamic";
/** A SOCKS proxy: the destination is picked per connection. */
export const isSocks = (kind: PortForwardKind) => kind === "dynamic" || kind === "remoteDynamic";

export function isLoopback(addr: string | null | undefined): boolean {
  if (!addr) return true;
  const a = addr.trim().toLowerCase().replace(/^\[|\]$/g, "");
  return a === "localhost" || a === "::1" || /^127\.\d+\.\d+\.\d+$/.test(a);
}

export function isAllInterfaces(addr: string | null | undefined): boolean {
  const a = (addr ?? "").trim().replace(/^\[|\]$/g, "");
  return a === "0.0.0.0" || a === "::" || a === "*";
}

/** `host:port`, bracketing IPv6 literals. */
export function hostPort(host: string, port: number | string): string {
  return host.includes(":") && !host.startsWith("[") ? `[${host}]:${port}` : `${host}:${port}`;
}

export function machineName(id: HostId | null): string {
  return id ? (hosts.get(id)?.name ?? "Unknown host") : "this computer";
}

/** Where a forward listens, e.g. "localhost:5432", "0.0.0.0:8080", "prod-web-01:8080". */
export function listenLabel(f: PortForward): string {
  const port = f.bindPort || "auto";
  if (listensRemotely(f.kind)) {
    const host = machineName(f.hostId);
    return isLoopback(f.bindAddress) ? hostPort(host, port) : `${hostPort(f.bindAddress!, port)} on ${host}`;
  }
  return hostPort(isLoopback(f.bindAddress) ? "localhost" : f.bindAddress!, port);
}

export interface Route {
  /** Listening side. */
  from: string;
  /** Destination (null for SOCKS). */
  to: string | null;
  /** Machine the connection to the target is made from, when it isn't obvious from `to`. */
  via: string | null;
  /** `via` with its preposition, e.g. "via bastion", "on this computer". */
  viaLabel: string | null;
  /** The whole thing as one line (tooltips, screen readers, copy). */
  text: string;
}

/**
 * Compact human description:
 *   local     localhost:5432 → db.internal:5432 via bastion
 *   dynamic   SOCKS5 on localhost:1080 through prod-web-01
 *   remoteDynamic  SOCKS5 on prod-web-01:1080 through this computer
 *   remote    prod-web-01:8080 → raspberrypi:80          (machine-to-machine)
 *             staging-api:8080 → localhost:5173 on this computer
 */
export function describeRoute(f: PortForward): Route {
  const from = listenLabel(f);
  if (isSocks(f.kind)) {
    const via = f.kind === "dynamic" ? machineName(f.hostId) : machineName(f.viaHostId);
    return { from: `SOCKS5 on ${from}`, to: null, via, viaLabel: `through ${via}`, text: `SOCKS5 proxy on ${from} through ${via}` };
  }
  const target = f.targetHost ?? "localhost";
  const tport = f.targetPort ?? "?";
  if (f.kind === "local") {
    const via = machineName(f.hostId);
    const to = hostPort(target, tport);
    return { from, to, via, viaLabel: `via ${via}`, text: `${from} → ${to} via ${via}` };
  }
  // Remote: the target is reached from viaHostId or this computer.
  const origin = f.viaHostId ? machineName(f.viaHostId) : null;
  if (isLoopback(target)) {
    if (origin) {
      const to = hostPort(origin, tport);
      return { from, to, via: null, viaLabel: null, text: `${from} → ${to}` };
    }
    const to = hostPort(target, tport);
    return { from, to, via: "this computer", viaLabel: "on this computer", text: `${from} → ${to} on this computer` };
  }
  const to = hostPort(target, tport);
  const via = origin ?? "this computer";
  return { from, to, via, viaLabel: `via ${via}`, text: `${from} → ${to} via ${via}` };
}

/** Address a client should use: the live one when running, else the configured one. */
export function forwardAddress(f: PortForward, s?: PortForwardStatus): string {
  if (s?.state === "active" && !listensRemotely(f.kind)) return s.address;
  if (listensRemotely(f.kind)) return hostPort(f.bindAddress ?? "localhost", f.bindPort);
  return hostPort(f.bindAddress ?? "127.0.0.1", f.bindPort || 0);
}

const WEB_PORTS = new Set([80, 443, 3000, 3001, 4000, 4200, 5000, 5173, 5601, 8000, 8008, 8080, 8081, 8088, 8443, 8888, 9000, 9090, 15672]);
const TLS_PORTS = new Set([443, 8443]);

/** A URL to open for local forwards to web-looking services (needs the live port). */
export function browserUrl(f: PortForward, s: PortForwardStatus): string | null {
  if (f.kind !== "local" || s.state !== "active") return null;
  const port = Number(s.address.slice(s.address.lastIndexOf(":") + 1));
  const looksWeb = /^https?:\/\//i.test(f.targetHost ?? "") || WEB_PORTS.has(f.targetPort ?? -1) || WEB_PORTS.has(f.bindPort);
  if (!looksWeb || !Number.isFinite(port) || port <= 0) return null;
  const tls = TLS_PORTS.has(f.targetPort ?? -1) || /^https:/i.test(f.targetHost ?? "");
  const host = isAllInterfaces(f.bindAddress) || isLoopback(f.bindAddress) ? "localhost" : f.bindAddress!;
  return `${tls ? "https" : "http"}://${hostPort(host, port)}/`;
}

export async function copyAddress(f: PortForward) {
  const addr = forwardAddress(f, forwards.status(f.id));
  try {
    await writeClipboard(addr);
    toasts.success("Copied address", addr);
  } catch {
    toasts.show("error", "Couldn't access the clipboard");
  }
}

// ----------------------------------------------------------------- editing

export function draftOf(f: PortForward): PortForwardDraft {
  return {
    name: f.name,
    kind: f.kind,
    hostId: f.hostId,
    bindAddress: f.bindAddress,
    bindPort: f.bindPort,
    targetHost: f.targetHost,
    targetPort: f.targetPort,
    viaHostId: f.viaHostId,
    autoStart: f.autoStart,
  };
}

/** Opens the rule editor for a new rule, optionally prefilled. */
export function newForward(initial?: Partial<PortForwardDraft>) {
  ui.forwardEditor = { mode: "create", initial };
}

export function editForward(f: PortForward) {
  if (f.source === "app") ui.forwardEditor = { mode: "edit", id: f.id };
  else duplicateForward(f);
}

/** Prefills the editor with a copy (ssh_config rules: "Duplicate as Patchbay rule"). */
export function duplicateForward(f: PortForward) {
  const base = f.source === "sshConfig" ? `${f.name} (${machineName(f.hostId)})` : `${f.name} copy`;
  newForward({ ...draftOf(f), name: forwards.uniqueName(base), autoStart: false });
}

export async function deleteForward(f: PortForward) {
  const running = isRunning(forwards.status(f.id));
  const ok = await ui.confirm({
    title: `Delete “${f.name}”?`,
    message: running
      ? "The forward is stopped and the rule removed. Connections through it are closed."
      : "The rule is removed from Patchbay. This can't be undone.",
    confirmLabel: "Delete rule",
    danger: true,
  });
  if (!ok) return;
  if (await forwards.remove(f.id)) toasts.success(`Deleted “${f.name}”`);
}

export function openForwards(hostId: HostId | null) {
  tabs.openForwards(hostId);
}

export function forwardMenuItems(f: PortForward, { showHost = true } = {}): MenuItem[] {
  const s = forwards.status(f.id);
  const running = isRunning(s);
  const url = browserUrl(f, s);
  const items: MenuItem[] = [
    running
      ? { label: "Stop", icon: Square, onSelect: () => void forwards.stop(f.id) }
      : { label: s.state === "failed" ? "Try again" : "Start", icon: Play, onSelect: () => void forwards.start(f.id) },
  ];
  if (url) items.push({ label: "Open in browser", icon: ExternalLink, onSelect: () => void openInBrowser(url) });
  items.push({ label: "Copy address", icon: Copy, hint: forwardAddress(f, s), onSelect: () => void copyAddress(f) });
  items.push({ separator: true });
  if (f.source === "app") {
    items.push({ label: "Edit…", icon: Pencil, onSelect: () => editForward(f) });
    items.push({ label: "Duplicate…", icon: CopyPlus, onSelect: () => duplicateForward(f) });
  } else {
    items.push({ label: "Duplicate as Patchbay rule…", icon: CopyPlus, onSelect: () => duplicateForward(f) });
  }
  if (showHost && hosts.get(f.hostId)) {
    items.push({ label: `Open ${machineName(f.hostId)}`, icon: Server, onSelect: () => tabs.openHost(f.hostId) });
  }
  if (f.source === "app") {
    items.push({ separator: true });
    items.push({ label: "Delete…", icon: Trash2, danger: true, onSelect: () => void deleteForward(f) });
  }
  return items;
}

export function showForwardMenu(f: PortForward, at: { x: number; y: number } | HTMLElement, opts?: { showHost?: boolean }) {
  ui.openMenu(`${f.name} actions`, forwardMenuItems(f, opts), at);
}
