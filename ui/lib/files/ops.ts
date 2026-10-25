// User-level file actions shared by the file list, toolbar, context menus,
// drag-and-drop and keyboard shortcuts.

import ArrowLeftRight from "@lucide/svelte/icons/arrow-left-right";
import ClipboardPaste from "@lucide/svelte/icons/clipboard-paste";
import Copy from "@lucide/svelte/icons/copy";
import Download from "@lucide/svelte/icons/download";
import Eye from "@lucide/svelte/icons/eye";
import FilePen from "@lucide/svelte/icons/file-pen";
import FilePlus from "@lucide/svelte/icons/file-plus";
import FolderOpen from "@lucide/svelte/icons/folder-open";
import FolderPlus from "@lucide/svelte/icons/folder-plus";
import Link from "@lucide/svelte/icons/link";
import Pencil from "@lucide/svelte/icons/pencil";
import RefreshCw from "@lucide/svelte/icons/refresh-cw";
import Shield from "@lucide/svelte/icons/shield";
import SquareArrowOutUpRight from "@lucide/svelte/icons/square-arrow-out-up-right";
import SquareTerminal from "@lucide/svelte/icons/square-terminal";
import Trash2 from "@lucide/svelte/icons/trash-2";
import Upload from "@lucide/svelte/icons/upload";
import { isTauri } from "../api";
import { attempt, call } from "../stores/backend";
import { connections } from "../stores/connections.svelte";
import type { FileDrag } from "../stores/dnd.svelte";
import { edits } from "../stores/edits.svelte";
import { filePrefs, files, type FileBrowser } from "../stores/files.svelte";
import { hosts } from "../stores/hosts.svelte";
import { tabs, type FilesTab, type Placement } from "../stores/tabs.svelte";
import { toasts } from "../stores/toasts.svelte";
import { transfers } from "../stores/transfers.svelte";
import { ui, type MenuItem } from "../stores/ui.svelte";
import type { FileEntry, FileRef, HostId } from "../types";
import { isDirLike } from "../util/fileIcons";
import { plural } from "../util/format";
import { basename, dirname, join } from "../util/paths";
import { modKey } from "../util/platform";
import { openTerminal, terminalUnavailable } from "../terminal/ops";
import { writeClipboard } from "../util/clipboard";

/** "Open terminal here" for a pane: opens a terminal tab (same group) in `dir`. */
export function terminalHereItem(b: FileBrowser, dir: string | null = b.path): MenuItem {
  const reason = terminalUnavailable(b.hostId);
  return {
    label: "Open terminal here",
    icon: SquareTerminal,
    disabled: !dir || !!reason,
    hint: reason ? "SFTP only" : undefined,
    onSelect: () => dir && openTerminal(b.hostId, { cwd: dir }),
  };
}

export function machineName(hostId: HostId | null): string {
  return hostId ? (hosts.get(hostId)?.name ?? "Unknown host") : "This computer";
}

/** "3 items" or the single item's name, quoted. */
function describe(entries: FileEntry[]): string {
  return entries.length === 1 ? `“${entries[0].name}”` : plural(entries.length, "item");
}

// ------------------------------------------------------------------ tabs

export interface OpenFilesOptions {
  path?: string | null;
  /** Focus an existing files tab for this machine instead of opening one. */
  reuse?: boolean;
  group?: Placement;
}

/** Opens a files tab, connecting first for remote hosts. */
export function openFiles(hostId: HostId | null, { path = null, reuse = false, group }: OpenFilesOptions = {}) {
  if (hostId) {
    const phase = connections.get(hostId).phase;
    if (phase === "idle" || phase === "error") void connections.connect(hostId);
  }
  if (reuse) {
    const existing =
      tabs.list.find((t) => t.kind === "files" && t.hostId === hostId && tabs.isVisible(t.id)) ??
      tabs.list.find((t) => t.kind === "files" && t.hostId === hostId);
    if (existing) {
      if (group !== undefined) tabs.moveToGroup(existing.id, group);
      tabs.activate(existing.id);
      return existing;
    }
  }
  return tabs.open({ kind: "files", hostId, path }, { group });
}

/** Opens a copy of a files tab at the same location, optionally in another group. */
export function duplicateFilesTab(tab: FilesTab, group?: Placement) {
  return tabs.open({ kind: "files", hostId: tab.hostId, path: tab.path }, { group });
}

// -------------------------------------------------------------- creating

export function startCreate(b: FileBrowser, kind: "dir" | "file") {
  if (!b.path || b.search) return;
  b.renaming = null;
  b.creating = kind;
}

export async function create(b: FileBrowser, kind: "dir" | "file", name: string): Promise<boolean> {
  const n = name.trim();
  if (!b.path || !n) return false;
  const bad = validateName(n);
  if (bad) {
    toasts.show("error", bad);
    return false;
  }
  const path = join(b.path, n);
  try {
    await call((be) => (kind === "dir" ? be.fsMkdir(b.hostId, path) : be.fsCreateFile(b.hostId, path)), {
      action: kind === "dir" ? "Couldn't create folder" : "Couldn't create file",
    });
  } catch {
    return false;
  }
  b.creating = null;
  await b.refresh();
  const created = b.listing?.entries.find((e) => e.name === n);
  if (created) {
    b.selectOnly(created.path);
    b.revealTick++;
  }
  return true;
}

function validateName(name: string): string | null {
  if (name === "." || name === "..") return "That name is reserved";
  if (/[/\\]/.test(name)) return "Names can't contain / or \\";
  return null;
}

// --------------------------------------------------------------- rename

export function startRename(b: FileBrowser, entry: FileEntry) {
  if (b.search) return;
  b.creating = null;
  b.selectOnly(entry.path);
  b.renaming = entry.path;
}

export async function rename(b: FileBrowser, entry: FileEntry, name: string): Promise<boolean> {
  const n = name.trim();
  if (!n || n === entry.name) {
    b.renaming = null;
    return true;
  }
  const bad = validateName(n);
  if (bad) {
    toasts.show("error", bad);
    return false;
  }
  const parent = dirname(entry.path) ?? b.path ?? "/";
  const to = join(parent, n);
  try {
    await call((be) => be.fsRename(b.hostId, entry.path, to), { action: `Couldn't rename “${entry.name}”` });
  } catch {
    return false;
  }
  b.renaming = null;
  await b.refresh();
  b.selectOnly(b.listing?.entries.find((e) => e.name === n)?.path ?? null);
  b.revealTick++;
  return true;
}

// --------------------------------------------------------------- delete

export async function remove(b: FileBrowser, entries: FileEntry[]) {
  if (!entries.length) return;
  const dirs = entries.filter((e) => e.kind === "dir").length;
  const ok = await ui.confirm({
    title: entries.length === 1 ? `Delete ${describe(entries)}?` : `Delete ${plural(entries.length, "item")}?`,
    message:
      (dirs ? "Folders are deleted with everything inside them. " : "") +
      `This permanently deletes from ${machineName(b.hostId)} and can't be undone.`,
    confirmLabel: "Delete",
    danger: true,
    details: entries.map((e) => (isDirLike(e) && e.kind === "dir" ? `${e.name}/` : e.name)),
  });
  if (!ok) return;
  try {
    await call(
      (be) =>
        be.fsDelete(
          b.hostId,
          entries.map((e) => e.path),
        ),
      { action: "Couldn't delete" },
    );
    toasts.success(entries.length === 1 ? `Deleted ${describe(entries)}` : `Deleted ${plural(entries.length, "item")}`);
  } catch {
    // toasted
  }
  await b.refresh();
  b.focusTick++;
}

// ------------------------------------------------------------ transfers

export async function transfer(sourceHost: HostId | null, entries: FileEntry[], destination: FileRef) {
  if (!entries.length) return;
  await transfers.start({
    sources: entries.map((e) => ({ hostId: sourceHost, path: e.path })),
    destination,
  });
}

/** Moves entries to another folder on the same machine. */
export async function move(hostId: HostId | null, entries: FileEntry[], destDir: string) {
  const failed: string[] = [];
  let moved = 0;
  for (const e of entries) {
    const to = join(destDir, e.name);
    if (to === e.path) continue;
    try {
      await call((b) => b.fsRename(hostId, e.path, to), { action: `Couldn't move “${e.name}”` });
      moved++;
    } catch {
      failed.push(e.name);
    }
  }
  const sourceDirs = new Set(entries.map((e) => dirname(e.path) ?? "/"));
  for (const d of sourceDirs) files.refreshDir(hostId, d);
  files.refreshDir(hostId, destDir);
  if (moved) toasts.success(`Moved ${moved === entries.length ? describe(entries) : plural(moved, "item")}`, `to ${basename(destDir)}`);
}

/** Performs a file drop from one pane onto another (or a folder in the same pane). */
export async function drop(drag: FileDrag) {
  const t = drag.target;
  if (!t) return;
  if (t.hostId === drag.hostId && !drag.copy) await move(t.hostId, drag.entries, t.dir);
  else await transfer(drag.hostId, drag.entries, { hostId: t.hostId, path: t.dir });
}

export function copyToClipboard(b: FileBrowser, entries: FileEntry[]) {
  if (!entries.length) return;
  files.setClipboard({ hostId: b.hostId, entries: entries.map((e) => ({ ...e })) });
  toasts.info(`Copied ${describe(entries)}`, `Paste into any files tab with ${modKey}+V to copy them there.`);
}

export async function paste(b: FileBrowser) {
  const clip = files.clipboard;
  if (!clip || !b.path) return;
  await transfer(clip.hostId, clip.entries, { hostId: b.hostId, path: b.path });
}

export async function copyToOther(b: FileBrowser, entries: FileEntry[], other: FileBrowser) {
  if (!other.path) return;
  await transfer(b.hostId, entries, { hostId: other.hostId, path: other.path });
}

/** Last folder used in a native file picker, so the next one starts there. */
let lastPickerDir: string | null = null;

async function pickerStart(fallback: "download" | "home"): Promise<string | undefined> {
  if (lastPickerDir) return lastPickerDir;
  const path = await import("@tauri-apps/api/path");
  try {
    return await (fallback === "download" ? path.downloadDir() : path.homeDir());
  } catch {
    return undefined;
  }
}

/** Downloads remote entries to a local folder (picked in the desktop app). */
export async function downloadTo(b: FileBrowser, entries: FileEntry[]) {
  let dir: string | null = null;
  if (isTauri) {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const picked = await open({
      directory: true,
      multiple: false,
      title: `Download ${describe(entries)} to…`,
      defaultPath: await pickerStart("download"),
    });
    dir = typeof picked === "string" ? picked : null;
    if (dir) lastPickerDir = dir;
  } else {
    // Browser preview: no native picker; use ~/Downloads of the mock machine.
    const home = await attempt((be) => be.fsHome(null), { toast: false });
    dir = home ? join(home, "Downloads") : null;
  }
  if (dir) await transfer(b.hostId, entries, { hostId: null, path: dir });
}

/** Uploads local files/folders (native picker) into the pane's directory. */
export async function upload(b: FileBrowser, directory = false) {
  if (!b.path) return;
  if (!isTauri) {
    toasts.info("Upload from the desktop app", "The file picker needs the Patchbay desktop app. Drag files between panes to copy them here.");
    return;
  }
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({
    multiple: true,
    directory,
    title: directory ? "Upload folders" : "Upload files",
    defaultPath: await pickerStart("home"),
  });
  const paths = picked == null ? [] : Array.isArray(picked) ? picked : [picked];
  if (!paths.length) return;
  lastPickerDir = directory ? paths[0] : (localParent(paths[0]) ?? lastPickerDir);
  await transfers.start({
    sources: paths.map((path) => ({ hostId: null, path })),
    destination: { hostId: b.hostId, path: b.path },
  });
}

export async function copyPaths(entries: FileEntry[]) {
  const text = entries.map((e) => e.path).join("\n");
  try {
    await writeClipboard(text);
    toasts.success(entries.length === 1 ? "Copied path" : `Copied ${entries.length} paths`, entries.length === 1 ? text : undefined);
  } catch {
    toasts.show("error", "Couldn't access the clipboard");
  }
}

export function editPermissions(b: FileBrowser, entries: FileEntry[]) {
  if (!entries.length) return;
  ui.permissions = {
    hostId: b.hostId,
    entries: entries.map((e) => ({ ...e })),
    onApplied: () => void b.refresh(),
  };
}

// ------------------------------------------------------------- opening

/** Enter / double-click: folders navigate, files open the preview. */
export function open(b: FileBrowser, entry: FileEntry) {
  if (b.search) {
    reveal(b, entry);
    return;
  }
  if (isDirLike(entry)) {
    void b.navigate(entry.path);
    return;
  }
  if (entry.kind === "symlink" && entry.targetKind == null) {
    toasts.show("error", `“${entry.name}” is a broken link`, entry.linkTarget ? `It points to ${entry.linkTarget}, which doesn't exist.` : undefined);
    return;
  }
  b.selectOnly(entry.path);
  setPreview(b, true);
}

/** Opens a file on this machine with its default app. */
export async function openWithDefaultApp(entry: FileEntry) {
  if (!isTauri) {
    toasts.info("Opens in the desktop app", `In the Patchbay desktop app, “${entry.name}” opens in its default app.`);
    return;
  }
  await attempt((be) => be.openLocal(entry.path), { action: `Couldn't open “${entry.name}”` });
}

/** "Edit in default app" (remote) / "Open with default app" (local) for a file. */
function openExternallyItem(b: FileBrowser, entry: FileEntry): MenuItem | null {
  if (isDirLike(entry) || (entry.kind === "symlink" && entry.targetKind == null) || entry.kind === "other") return null;
  const hostId = b.hostId;
  return hostId
    ? { label: "Edit in default app", icon: FilePen, onSelect: () => void edits.open(hostId, entry) }
    : { label: "Open with default app", icon: SquareArrowOutUpRight, onSelect: () => void openWithDefaultApp(entry) };
}

/** Search result → go to its folder and select it. */
export function reveal(b: FileBrowser, entry: FileEntry) {
  const dir = dirname(entry.path);
  if (!dir) return;
  b.cancelSearch();
  void b.navigate(dir, { select: entry.path }).then(() => b.focusTick++);
}

export function setPreview(b: FileBrowser, open: boolean) {
  b.previewOpen = open;
  filePrefs.previewOpen = open;
  filePrefs.save();
}

// ---------------------------------------------------------------- menus

export function entryMenu(b: FileBrowser, entries: FileEntry[]): MenuItem[] {
  const one = entries.length === 1 ? entries[0] : null;
  const others = files.otherPanes(b.tabId).filter((o) => o.path);
  const remote = b.hostId !== null;
  const items: MenuItem[] = [];
  if (one) {
    if (b.search) {
      items.push({ label: "Show in folder", icon: FolderOpen, hint: "Enter", onSelect: () => reveal(b, one) });
    } else if (isDirLike(one)) {
      items.push({ label: "Open", icon: FolderOpen, hint: "Enter", onSelect: () => open(b, one) });
      items.push({
        label: "Open in new tab",
        icon: FolderOpen,
        onSelect: () => tabs.open({ kind: "files", hostId: b.hostId, path: one.path }),
      });
    } else {
      items.push({ label: "Preview", icon: Eye, hint: "Space", onSelect: () => open(b, one) });
    }
    const external = openExternallyItem(b, one);
    if (external) items.push(external);
    items.push({ separator: true });
  }
  for (const other of others.slice(0, 3)) {
    items.push({
      label: `Copy to ${machineName(other.hostId)}: ${basename(other.path!)}`,
      icon: ArrowLeftRight,
      onSelect: () => void copyToOther(b, entries, other),
    });
  }
  if (remote) items.push({ label: "Download to…", icon: Download, onSelect: () => void downloadTo(b, entries) });
  items.push({ label: "Copy", icon: Copy, hint: `${modKey}+C`, onSelect: () => copyToClipboard(b, entries) });
  items.push({ separator: true });
  if (one && !b.search) items.push({ label: "Rename", icon: Pencil, hint: "F2", onSelect: () => startRename(b, one) });
  items.push({ label: "Permissions…", icon: Shield, onSelect: () => editPermissions(b, entries) });
  items.push({
    label: entries.length === 1 ? "Copy path" : "Copy paths",
    icon: Link,
    onSelect: () => void copyPaths(entries),
  });
  items.push({ separator: true });
  items.push({ label: "Delete…", icon: Trash2, danger: true, hint: "Del", onSelect: () => void remove(b, entries) });
  return items;
}

export function backgroundMenu(b: FileBrowser): MenuItem[] {
  const items: MenuItem[] = [
    { label: "New folder", icon: FolderPlus, hint: `${modKey}+Shift+N`, onSelect: () => startCreate(b, "dir"), disabled: !!b.search },
    { label: "New file", icon: FilePlus, onSelect: () => startCreate(b, "file"), disabled: !!b.search },
  ];
  if (b.hostId) items.push({ label: "Upload files…", icon: Upload, onSelect: () => void upload(b) });
  const clip = files.clipboard;
  items.push({
    label: clip ? `Paste ${clip.entries.length === 1 ? `“${clip.entries[0].name}”` : plural(clip.entries.length, "item")}` : "Paste",
    icon: ClipboardPaste,
    hint: `${modKey}+V`,
    disabled: !clip || !!b.search,
    onSelect: () => void paste(b),
  });
  items.push({ separator: true });
  items.push({
    label: "Copy path",
    icon: Link,
    disabled: !b.path,
    onSelect: () => b.path && void copyPaths([{ path: b.path } as FileEntry]),
  });
  items.push(terminalHereItem(b));
  items.push({ separator: true });
  items.push({ label: "Refresh", icon: RefreshCw, hint: "F5", onSelect: () => void b.refresh() });
  return items;
}

/** Context menu for a selection (or the background when `entries` is empty). */
export function showMenu(b: FileBrowser, entries: FileEntry[], at: { x: number; y: number } | HTMLElement) {
  if (entries.length) {
    const items = entryMenu(b, entries);
    if (!b.search) {
      const one = entries.length === 1 && isDirLike(entries[0]) ? entries[0] : null;
      items.splice(items.length - 2, 0, terminalHereItem(b, one ? one.path : b.path));
    }
    ui.openMenu(entries.length === 1 ? `${entries[0].name} actions` : `${entries.length} items actions`, items, at);
  } else {
    ui.openMenu("Folder actions", backgroundMenu(b), at);
  }
}

/** Parent folder of a native local path (either separator). */
function localParent(path: string): string | null {
  const i = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return i > 0 ? path.slice(0, i) : null;
}
