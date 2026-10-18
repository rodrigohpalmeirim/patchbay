// User-level terminal actions shared by the terminal view, context menus,
// keyboard shortcuts and the other entry points (overview, sidebar, files).

import ClipboardPaste from "@lucide/svelte/icons/clipboard-paste";
import Columns2 from "@lucide/svelte/icons/columns-2";
import Copy from "@lucide/svelte/icons/copy";
import Eraser from "@lucide/svelte/icons/eraser";
import FolderOpen from "@lucide/svelte/icons/folder-open";
import Maximize2 from "@lucide/svelte/icons/maximize-2";
import Minimize2 from "@lucide/svelte/icons/minimize-2";
import RotateCw from "@lucide/svelte/icons/rotate-cw";
import Rows2 from "@lucide/svelte/icons/rows-2";
import Search from "@lucide/svelte/icons/search";
import SquareCode from "@lucide/svelte/icons/square-code";
import TextSelect from "@lucide/svelte/icons/square-dashed-text";
import X from "@lucide/svelte/icons/x";
import { openFiles } from "../files/ops";
import { connections } from "../stores/connections.svelte";
import { hosts } from "../stores/hosts.svelte";
import { tabs, type TerminalTab } from "../stores/tabs.svelte";
import { terminals } from "../stores/terminals.svelte";
import { ui, type MenuItem } from "../stores/ui.svelte";
import type { HostId } from "../types";
import { tildify } from "../util/format";
import { keyLabel, type TermAction } from "./keys";
import { findLeaf, leaves, neighbor, newPane, removePane, splitPane, type Direction, type SplitDir } from "./layout";

/** Why a terminal can't be opened on this machine (null = it can, or we don't know yet). */
export function terminalUnavailable(hostId: HostId | null): string | null {
  if (!hostId) return null;
  const caps = connections.get(hostId).capabilities;
  return caps && !caps.exec ? "This server only allows SFTP" : null;
}

export interface OpenTerminalOptions {
  /** Start directory (null = home). */
  cwd?: string | null;
  group?: number | "other";
}

/** Opens a terminal tab, connecting first for remote hosts. */
export function openTerminal(hostId: HostId | null, { cwd = null, group }: OpenTerminalOptions = {}): TerminalTab {
  if (hostId) {
    const phase = connections.get(hostId).phase;
    if (phase === "idle" || phase === "error") void connections.connect(hostId);
  }
  const target = group === "other" ? (tabs.focused === 0 ? 1 : 0) : group;
  const pane = newPane(cwd);
  const tab = tabs.open(
    { kind: "terminal", hostId, cwd, layout: pane, focusedPane: pane.id, zoomedPane: null },
    { group: target },
  ) as TerminalTab;
  terminals.requestFocus(tab.id);
  return tab;
}

/** New terminal for the machine of the focused tab (this computer if none). */
export function newTerminalForActive() {
  const hostId = tabs.activeHostId;
  if (hostId && terminalUnavailable(hostId)) {
    openTerminal(null);
    return;
  }
  openTerminal(hostId);
}

export function duplicateTerminalTab(tab: TerminalTab, group?: number | "other") {
  const cwd = terminals.get(tab.focusedPane)?.cwd ?? tab.cwd;
  return openTerminal(tab.hostId, { cwd, group });
}

export function machineLabel(hostId: HostId | null): string {
  return hostId ? (hosts.get(hostId)?.name ?? "Unknown host") : "This computer";
}

// ----------------------------------------------------------------- panes

function liveTab(tabId: string): TerminalTab | null {
  const t = tabs.get(tabId);
  return t?.kind === "terminal" ? t : null;
}

export function focusPane(tab: TerminalTab, paneId: string, focusDom = true) {
  if (!findLeaf(tab.layout, paneId)) return;
  if (tab.focusedPane !== paneId) tab.focusedPane = paneId;
  if (focusDom) terminals.get(paneId)?.focus();
}

export function split(tab: TerminalTab, paneId: string, dir: SplitDir) {
  const leaf = findLeaf(tab.layout, paneId);
  if (!leaf) return;
  const cwd = terminals.get(paneId)?.cwd ?? leaf.cwd;
  const added = newPane(cwd);
  tab.layout = splitPane(tab.layout, paneId, dir, added);
  tab.zoomedPane = null;
  tab.focusedPane = added.id;
  terminals.requestFocus(tab.id);
}

export function closePane(tab: TerminalTab, paneId: string) {
  const live = liveTab(tab.id);
  if (!live || !findLeaf(live.layout, paneId)) return;
  // Pick where focus goes before the layout changes.
  const dirs: Direction[] = ["left", "up", "right", "down"];
  const next = dirs.map((d) => neighbor(live.layout, paneId, d)).find((id) => id) ?? null;
  const rest = removePane(live.layout, paneId);
  terminals.dispose(paneId);
  if (!rest) {
    tabs.close(live.id);
    return;
  }
  live.layout = rest;
  if (live.zoomedPane === paneId) live.zoomedPane = null;
  if (live.focusedPane === paneId) live.focusedPane = next && findLeaf(rest, next) ? next : leaves(rest)[0].id;
  terminals.requestFocus(live.id);
}

export function moveFocus(tab: TerminalTab, dir: Direction) {
  if (tab.zoomedPane) return;
  const id = neighbor(tab.layout, tab.focusedPane, dir);
  if (id) focusPane(tab, id);
}

export function toggleZoom(tab: TerminalTab, paneId = tab.focusedPane) {
  if (leaves(tab.layout).length < 2) return;
  tab.zoomedPane = tab.zoomedPane ? null : paneId;
  tab.focusedPane = paneId;
  terminals.requestFocus(tab.id);
}

export function openFind(paneId: string) {
  const s = terminals.get(paneId);
  if (!s) return;
  s.findOpen = true;
  s.findTick++;
}

export function openSnippets(tab: TerminalTab) {
  terminals.paletteFor = tab.id;
}

/** Runs a terminal shortcut / menu action against a pane. */
export function runAction(tab: TerminalTab, paneId: string, action: TermAction) {
  const s = terminals.get(paneId);
  switch (action) {
    case "copy":
      void s?.copy();
      break;
    case "paste":
      void s?.pasteFromClipboard();
      break;
    case "find":
      openFind(paneId);
      break;
    case "snippets":
      focusPane(tab, paneId, false);
      openSnippets(tab);
      break;
    case "splitRight":
      split(tab, paneId, "row");
      break;
    case "splitDown":
      split(tab, paneId, "column");
      break;
    case "closePane":
      closePane(tab, paneId);
      break;
    case "zoom":
      toggleZoom(tab, paneId);
      break;
    case "clear":
      s?.clear();
      break;
    case "selectAll":
      s?.selectAll();
      break;
    case "focus-left":
    case "focus-right":
    case "focus-up":
    case "focus-down":
      moveFocus(tab, action.slice(6) as Direction);
      break;
  }
}

/** Context menu for a pane (right click, toolbar "More"). */
export function paneMenu(tab: TerminalTab, paneId: string): MenuItem[] {
  const s = terminals.get(paneId);
  const running = s?.status === "running";
  const multi = leaves(tab.layout).length > 1;
  const items: MenuItem[] = [
    { label: "Copy", icon: Copy, hint: keyLabel("copy"), disabled: !s?.hasSelection, onSelect: () => runAction(tab, paneId, "copy") },
    { label: "Paste", icon: ClipboardPaste, hint: keyLabel("paste"), disabled: !running, onSelect: () => runAction(tab, paneId, "paste") },
    { label: "Select all", icon: TextSelect, hint: keyLabel("selectAll"), onSelect: () => runAction(tab, paneId, "selectAll") },
    { separator: true },
    { label: "Find…", icon: Search, hint: keyLabel("find"), onSelect: () => runAction(tab, paneId, "find") },
    { label: "Snippets…", icon: SquareCode, hint: keyLabel("snippets"), disabled: !running, onSelect: () => runAction(tab, paneId, "snippets") },
    { label: "Clear", icon: Eraser, hint: keyLabel("clear"), onSelect: () => runAction(tab, paneId, "clear") },
    { separator: true },
    { label: "Split right", icon: Columns2, hint: keyLabel("splitRight"), onSelect: () => runAction(tab, paneId, "splitRight") },
    { label: "Split down", icon: Rows2, hint: keyLabel("splitDown"), onSelect: () => runAction(tab, paneId, "splitDown") },
  ];
  if (multi) {
    const zoomed = tab.zoomedPane === paneId;
    items.push({
      label: zoomed ? "Restore pane size" : "Maximize pane",
      icon: zoomed ? Minimize2 : Maximize2,
      hint: keyLabel("zoom"),
      onSelect: () => runAction(tab, paneId, "zoom"),
    });
  }
  const cwd = s?.cwd;
  if (cwd && !(tab.hostId && connections.get(tab.hostId).capabilities?.sftp === false)) {
    items.push({ separator: true });
    items.push({
      label: "Open files here",
      icon: FolderOpen,
      onSelect: () => openFiles(tab.hostId, { path: cwd }),
    });
  }
  if (s && (s.status === "exited" || s.status === "failed")) {
    items.push({ separator: true });
    items.push({ label: "Restart shell", icon: RotateCw, hint: "Enter", onSelect: () => s.restart() });
  }
  items.push({ separator: true });
  items.push({ label: multi ? "Close pane" : "Close terminal", icon: X, hint: keyLabel("closePane"), onSelect: () => closePane(tab, paneId) });
  return items;
}

export function showPaneMenu(tab: TerminalTab, paneId: string, at: { x: number; y: number } | HTMLElement) {
  ui.openMenu("Terminal actions", paneMenu(tab, paneId), at);
}

/** Short display form of a shell directory: `~` for home, `C:/x` for Windows remotes. */
export function displayPath(path: string, hostId: HostId | null): string {
  const home = hostId ? connections.get(hostId).capabilities?.home : null;
  let p = path;
  if (home) {
    const h = home.replace(/\\/g, "/").replace(/^([A-Za-z]:)/, "/$1").replace(/\/$/, "");
    if (p === h) return "~";
    if (p.startsWith(h + "/")) p = "~" + p.slice(h.length);
  } else if (!hostId) {
    p = tildify(p);
  }
  return p.replace(/^\/([A-Za-z]:)/, "$1");
}

/** Tab title: the shell's window title, else its directory, else the machine. */
export function terminalTitle(tab: TerminalTab): string {
  const s = terminals.get(tab.focusedPane);
  if (s?.title) return s.title;
  // Restored tabs show their remembered directory before the shell starts.
  const cwd = s?.cwd ?? (s ? null : findLeaf(tab.layout, tab.focusedPane)?.cwd);
  if (cwd) return displayPath(cwd, tab.hostId);
  return tab.hostId ? machineLabel(tab.hostId) : "Terminal";
}
