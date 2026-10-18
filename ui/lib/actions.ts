// User-level host actions shared by the sidebar, tabs and overview.

import Cable from "@lucide/svelte/icons/cable";
import Copy from "@lucide/svelte/icons/copy";
import CopyPlus from "@lucide/svelte/icons/copy-plus";
import FileText from "@lucide/svelte/icons/file-text";
import FolderOpen from "@lucide/svelte/icons/folder-open";
import PanelTop from "@lucide/svelte/icons/panel-top";
import Pencil from "@lucide/svelte/icons/pencil";
import Plug from "@lucide/svelte/icons/plug";
import SquareTerminal from "@lucide/svelte/icons/square-terminal";
import Trash2 from "@lucide/svelte/icons/trash-2";
import Unplug from "@lucide/svelte/icons/unplug";
import X from "@lucide/svelte/icons/x";
import { openFiles } from "./files/ops";
import { openTerminal, terminalUnavailable } from "./terminal/ops";
import { connections } from "./stores/connections.svelte";
import { forwards } from "./stores/forwards.svelte";
import { hosts } from "./stores/hosts.svelte";
import { tabs } from "./stores/tabs.svelte";
import { toasts } from "./stores/toasts.svelte";
import { ui, type MenuItem } from "./stores/ui.svelte";
import type { Host, HostDraft } from "./types";
import { sshCommand } from "./util/format";
import { writeClipboard } from "./util/clipboard";

export function openHost(host: Host) {
  tabs.openHost(host.id);
}

export function connectHost(host: Host, { open = true } = {}) {
  if (open) tabs.openHost(host.id);
  void connections.connect(host.id);
}

export async function disconnectHost(host: Host) {
  const running = forwards.activeFor(host.id);
  if (running.length && connections.get(host.id).phase === "connected") {
    const ok = await ui.confirm({
      title: `Disconnect from ${host.name}?`,
      message:
        running.length === 1
          ? `The port forward “${running[0].name}” goes through this connection and will stop.`
          : `${running.length} port forwards go through this connection and will stop.`,
      confirmLabel: "Disconnect",
      details: running.length > 1 ? running.map((f) => f.name) : undefined,
    });
    if (!ok) return;
  }
  void connections.disconnect(host.id);
}

export function newHost(initial?: Partial<HostDraft>) {
  ui.editor = { mode: "create", initial };
}

export function editHost(host: Host) {
  ui.editor = host.source === "app" ? { mode: "edit", hostId: host.id } : { mode: "view", hostId: host.id };
}

/** Prefills the editor with a copy of `host` (used for read-only ssh_config hosts). */
export function duplicateHost(host: Host) {
  let name = `${host.name}-copy`;
  for (let i = 2; hosts.findByName(name); i++) name = `${host.name}-copy-${i}`;
  newHost({
    name,
    hostname: host.hostname,
    port: host.port === 22 ? null : host.port,
    user: host.user,
    identityFiles: [...host.identityFiles],
    proxyJump: host.proxyJump,
    group: host.group,
    tags: [...host.tags],
  });
}

export async function deleteHost(host: Host) {
  const ok = await ui.confirm({
    title: `Delete ${host.name}?`,
    message: "The host is removed from Patchbay. Keys, known_hosts entries and saved passwords are kept.",
    confirmLabel: "Delete host",
    danger: true,
  });
  if (!ok) return;
  if (connections.isActive(host.id)) await connections.disconnect(host.id);
  if (await hosts.remove(host.id)) {
    tabs.closeForHost(host.id);
    connections.forget(host.id);
    toasts.success(`Deleted ${host.name}`);
  }
}

export async function copySshCommand(host: Host) {
  try {
    await writeClipboard(sshCommand(host));
    toasts.success("Copied SSH command", sshCommand(host));
  } catch {
    toasts.show("error", "Couldn't access the clipboard");
  }
}

export function hostMenuItems(host: Host, { includeOpen = true } = {}): MenuItem[] {
  const phase = connections.get(host.id).phase;
  const items: MenuItem[] = [];
  if (includeOpen) items.push({ label: "Open", icon: PanelTop, onSelect: () => openHost(host) });
  if (phase === "connected") {
    items.push({ label: "Disconnect", icon: Unplug, onSelect: () => void disconnectHost(host) });
  } else if (phase === "idle" || phase === "error") {
    items.push({ label: "Connect", icon: Plug, onSelect: () => connectHost(host) });
  } else {
    items.push({ label: "Cancel connecting", icon: X, onSelect: () => void disconnectHost(host) });
  }
  items.push({ label: "Browse files", icon: FolderOpen, onSelect: () => openFiles(host.id, { reuse: true }) });
  const noShell = terminalUnavailable(host.id);
  items.push({
    label: "Open terminal",
    icon: SquareTerminal,
    disabled: !!noShell,
    hint: noShell ? "SFTP only" : undefined,
    onSelect: () => openTerminal(host.id),
  });
  items.push({ label: "Port forwards", icon: Cable, onSelect: () => tabs.openForwards(host.id) });
  items.push({ separator: true });
  if (host.source === "app") {
    items.push({ label: "Edit…", icon: Pencil, onSelect: () => editHost(host) });
    items.push({ label: "Duplicate…", icon: CopyPlus, onSelect: () => duplicateHost(host) });
  } else {
    items.push({ label: "View details…", icon: FileText, onSelect: () => editHost(host) });
    items.push({
      label: "Duplicate as Patchbay host…",
      icon: CopyPlus,
      onSelect: () => duplicateHost(host),
    });
  }
  items.push({ label: "Copy SSH command", icon: Copy, onSelect: () => void copySshCommand(host) });
  if (host.source === "app") {
    items.push({ separator: true });
    items.push({ label: "Delete…", icon: Trash2, danger: true, onSelect: () => void deleteHost(host) });
  }
  return items;
}
