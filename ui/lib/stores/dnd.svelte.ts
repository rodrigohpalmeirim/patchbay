// Drag and drop.
//
// Internal drags (file rows between/within panes, tabs between strips/groups)
// use pointer events rather than HTML5 DnD: Tauri's native file-drop handler
// (`dragDropEnabled`, on by default) disables HTML5 DnD inside the webview on
// Windows, and pointer drags give us full control over the ghost and hit
// testing. Drop targets are found with `elementFromPoint` + data attributes:
//   [data-pane="<tabId>"]        a files pane (drop = its current directory)
//   [data-dir-path="<path>"]     a folder row inside a pane (drop = that folder)
//   [data-strip-group="<id>"]    a tab strip, [data-tab-id] tabs inside it
//   [data-group-panel="<id>"]    a group's panel (edges = split it, middle = move there)
//   [data-terminal-pane="<id>"]  a terminal pane (edges = join that terminal tab there)
//
// Drops from the OS arrive through Tauri's webview drag-drop events (local
// paths + physical position); in a plain browser only HTML5 file drags exist,
// without paths, so we just highlight and explain.
//
// Dragging rows out of the window hands over to a native OS drag
// (dragout.svelte.ts prepares local paths). The pointer is captured when a
// file drag starts so moves outside the window keep arriving; once it is
// outside with the button held and the paths are ready, the internal drag
// ends and the native one starts. The webview usually never sees the
// matching pointerup after that, which is why the internal drag is torn
// down first. If the native drag comes back into the window and is dropped
// on a pane, the original entries are copied there (not the temporary copy).

import { getBackend, isTauri, type Unlisten } from "../api";
import type { FileEntry, HostId } from "../types";
import { dirname, isWithin, normalize } from "../util/paths";
import { isMac } from "../util/platform";
import { sentence, toCommandError } from "../util/errors";
import { DragOut, dragIcon } from "./dragout.svelte";
import { files } from "./files.svelte";
import type { Direction } from "../util/splitTree";
import { canJoin, joinPanes, movePaneToTab } from "../terminal/ops";
import { GROUP_MIN_H, GROUP_MIN_W, tabs } from "./tabs.svelte";
import { toasts } from "./toasts.svelte";
import { transfers } from "./transfers.svelte";

export interface DropTarget {
  paneId: string;
  hostId: HostId | null;
  /** Destination directory. */
  dir: string;
  /** Folder row under the pointer (null = the pane's current directory). */
  rowPath: string | null;
}

export interface FileDrag {
  sourcePane: string;
  hostId: HostId | null;
  entries: FileEntry[];
  x: number;
  y: number;
  target: DropTarget | null;
  /** Copy instead of move (same machine only). */
  copy: boolean;
  /** The pointer is outside the window with the button held (drag-out). */
  outside: boolean;
}

/** A native drag of our own entries in progress (see `Dnd.native`). */
export interface NativeDrag {
  drag: FileDrag;
  onDrop: (drag: FileDrag) => void;
}

export type TabDropTarget =
  | { kind: "strip"; group: string; index: number; x: number }
  /** Into a group (`edge` null), or a new group split off it on that side; `rect` is the highlighted area. */
  | { kind: "group"; group: string; edge: Direction | null; rect: DropRect }
  /** Terminal panes join terminal tab `tab` next to `pane`. */
  | { kind: "pane"; tab: string; pane: string; edge: Direction; rect: DropRect };

export interface DropRect {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** A tab, or with `pane` one pane of a terminal tab, being dragged. */
export interface TabDrag {
  id: string;
  pane: string | null;
  x: number;
  y: number;
  target: TabDropTarget | null;
}

export interface OsDrag {
  x: number;
  y: number;
  count: number;
  target: DropTarget | null;
  /** Our own drag-out came back into the window. */
  own: boolean;
}

/** Copy modifier: Option on macOS, Ctrl elsewhere (like Finder/Explorer). */
export const copyModifier = (e: { altKey: boolean; ctrlKey: boolean }) => (isMac ? e.altKey : e.ctrlKey);
export const COPY_KEY_LABEL = isMac ? "⌥" : "Ctrl";

const THRESHOLD = 5;

const isOutside = (ev: PointerEvent) =>
  ev.clientX < 0 || ev.clientY < 0 || ev.clientX >= window.innerWidth || ev.clientY >= window.innerHeight;

/** Finds the files pane / folder row under a viewport point. */
export function hitTest(x: number, y: number): DropTarget | null {
  const el = document.elementFromPoint(x, y);
  const pane = el?.closest<HTMLElement>("[data-pane]");
  const paneId = pane?.dataset.pane;
  if (!paneId) return null;
  const b = files.get(paneId);
  if (!b?.path || b.stale || (b.error && !b.listing)) return null;
  const row = el?.closest<HTMLElement>("[data-dir-path]");
  const rowPath = row?.dataset.dirPath ?? null;
  return { paneId, hostId: b.hostId, dir: rowPath ?? b.path, rowPath };
}

/** Is dropping `drag` on its current target meaningful? */
export function dropAllowed(drag: FileDrag): boolean {
  const t = drag.target;
  if (!t) return false;
  if (t.hostId !== drag.hostId) return true;
  // Same machine: can't put a folder inside itself.
  if (drag.entries.some((e) => isWithin(e.path, t.dir))) return false;
  // Moving into the folder the items are already in does nothing.
  if (!drag.copy && drag.entries.every((e) => dirname(e.path) === normalize(t.dir))) return false;
  return true;
}

/** Same-machine drop = move unless the copy modifier is held. */
export function dropIsMove(drag: FileDrag): boolean {
  return !!drag.target && drag.target.hostId === drag.hostId && !drag.copy;
}

class Dnd {
  file = $state<FileDrag | null>(null);
  tab = $state<TabDrag | null>(null);
  os = $state<OsDrag | null>(null);
  /** Local paths for dragging the current file drag out of the window. */
  out = $state<DragOut | null>(null);
  /** Our native drag-out in progress. */
  native: NativeDrag | null = null;
  /** Set right after a drag ends so the trailing click is ignored. */
  justDropped = false;

  /**
   * Arms a potential file drag from a pointerdown on a row. The drag starts
   * once the pointer moves a few pixels; `onDrop` performs the operation.
   */
  armFileDrag(
    e: PointerEvent,
    sourcePane: string,
    hostId: HostId | null,
    getEntries: () => FileEntry[],
    onDrop: (drag: FileDrag) => void,
  ) {
    const sx = e.clientX;
    const sy = e.clientY;
    const captureEl = e.currentTarget instanceof Element ? e.currentTarget : null;
    let pointerId: number | null = null;
    let started = false;
    let out: DragOut | null = null;
    const move = (ev: PointerEvent) => {
      if (!started) {
        if (Math.hypot(ev.clientX - sx, ev.clientY - sy) < THRESHOLD) return;
        const entries = getEntries();
        if (!entries.length) return stop();
        started = true;
        document.body.classList.add("dragging");
        try {
          captureEl?.setPointerCapture(ev.pointerId);
          pointerId = ev.pointerId;
        } catch {
          // Pointer already gone; the drag simply stays inside the window.
        }
        this.file = { sourcePane, hostId, entries, x: ev.clientX, y: ev.clientY, target: null, copy: copyModifier(ev), outside: false };
        out = new DragOut(hostId, entries, () => handOff());
        this.out = out;
      }
      const f = this.file;
      if (!f) return;
      f.x = ev.clientX;
      f.y = ev.clientY;
      f.copy = copyModifier(ev);
      f.outside = isOutside(ev) && (ev.buttons & 1) === 1;
      f.target = f.outside ? null : hitTest(ev.clientX, ev.clientY);
      if (f.outside) handOff();
    };
    /** Pointer outside and paths ready: continue as a native OS drag. */
    const handOff = () => {
      const f = this.file;
      if (!f?.outside || out?.status !== "ready" || !out.paths) return;
      const drag = $state.snapshot(f) as FileDrag;
      const paths = out.paths;
      out.detach();
      out = null;
      end();
      void this.#startNative(drag, paths, onDrop);
    };
    const key = (ev: KeyboardEvent) => {
      if (!this.file) return;
      if (ev.key === "Escape") {
        ev.preventDefault();
        ev.stopPropagation();
        end();
        return;
      }
      this.file.copy = copyModifier(ev);
    };
    const up = () => {
      const f = this.file;
      if (!started || !f) return stop();
      end();
      if (dropAllowed(f)) onDrop($state.snapshot(f) as FileDrag);
    };
    /** Ends a started drag; the drag-out copy goes unless it was handed off. */
    const end = () => {
      out?.release();
      out = null;
      this.out = null;
      this.file = null;
      stop();
      this.#suppressClick();
    };
    const stop = () => {
      if (pointerId !== null && captureEl?.hasPointerCapture(pointerId)) captureEl.releasePointerCapture(pointerId);
      pointerId = null;
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", cancel);
      window.removeEventListener("keydown", key, true);
      window.removeEventListener("keyup", key, true);
      document.body.classList.remove("dragging");
    };
    const cancel = () => {
      if (started) end();
      else stop();
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", cancel);
    window.addEventListener("keydown", key, true);
    window.addEventListener("keyup", key, true);
  }

  /**
   * Arms a tab drag (reorder, move to another group, split, join a terminal),
   * or with `pane` a drag of one pane of terminal tab `id`.
   */
  armTabDrag(e: PointerEvent, id: string, pane: string | null = null) {
    const sx = e.clientX;
    const sy = e.clientY;
    let started = false;
    const move = (ev: PointerEvent) => {
      if (!started) {
        if (Math.hypot(ev.clientX - sx, ev.clientY - sy) < THRESHOLD) return;
        started = true;
        document.body.classList.add("dragging");
        this.tab = { id, pane, x: ev.clientX, y: ev.clientY, target: null };
      }
      if (!this.tab) return;
      this.tab.x = ev.clientX;
      this.tab.y = ev.clientY;
      this.tab.target = tabTarget(id, pane, ev.clientX, ev.clientY);
    };
    const key = (ev: KeyboardEvent) => {
      if (ev.key === "Escape" && this.tab) {
        ev.preventDefault();
        ev.stopPropagation();
        this.tab = null;
        stop();
        this.#suppressClick();
      }
    };
    const up = () => {
      const t = this.tab;
      stop();
      if (!started || !t) return;
      this.tab = null;
      this.#suppressClick();
      if (t.target) drop(id, pane, t.target);
    };
    const stop = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("keydown", key, true);
      document.body.classList.remove("dragging");
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("keydown", key, true);
  }

  async #startNative(drag: FileDrag, paths: string[], onDrop: (drag: FileDrag) => void) {
    const native: NativeDrag = { drag, onDrop };
    this.native = native;
    const done = () => {
      if (this.native === native) this.native = null;
      this.os = null;
    };
    try {
      const backend = await getBackend();
      await backend.startNativeDrag(paths, dragIcon(drag.entries), done);
    } catch (e) {
      done();
      toasts.show(
        "error",
        "Couldn't drag out of Patchbay",
        `${sentence(toCommandError(e).message)} Use Download to… to copy files to this computer.`,
      );
    }
  }

  #suppressClick() {
    this.justDropped = true;
    setTimeout(() => (this.justDropped = false), 0);
  }
}

function drop(id: string, pane: string | null, target: TabDropTarget) {
  const tab = tabs.get(id);
  if (target.kind === "pane") {
    const into = tabs.get(target.tab);
    if (tab?.kind === "terminal" && into?.kind === "terminal") joinPanes(tab, pane, into, target.pane, target.edge);
  } else if (pane) {
    if (tab?.kind !== "terminal") return;
    if (target.kind === "strip") movePaneToTab(tab, pane, { group: target.group, index: target.index });
    else movePaneToTab(tab, pane, { group: target.edge ? { split: target.edge, of: target.group } : target.group });
  } else if (target.kind === "strip") tabs.move(id, target.index, target.group);
  else if (target.edge) tabs.moveToGroup(id, { split: target.edge, of: target.group });
  else tabs.moveToGroup(id, target.group);
}

/** Share of a group's width/height near each edge that splits it. */
const EDGE = 1 / 3;
/** The same over a terminal that the drag can join: the rest of it joins its panes. */
const EDGE_JOINABLE = 0.12;
/** Share of a pane's width/height near each edge that joins next to it. */
const PANE_EDGE = 0.3;

/** The edge of `r` nearest to (x, y), with its distance as a share of `r`'s width or height. */
function nearestEdge(r: DOMRect, x: number, y: number): [Direction, number] {
  const nx = (x - r.left) / r.width;
  const ny = (y - r.top) / r.height;
  const near: [Direction, number][] = [
    ["left", nx],
    ["right", 1 - nx],
    ["up", ny],
    ["down", 1 - ny],
  ];
  return near.reduce((a, b) => (b[1] < a[1] ? b : a));
}

/** The half of `r` on the `side` side. */
function half(r: DOMRect, side: Direction): DropRect {
  const out = { left: r.left, top: r.top, width: r.width, height: r.height };
  if (side === "left" || side === "right") out.width /= 2;
  if (side === "right") out.left += out.width;
  if (side === "up" || side === "down") out.height /= 2;
  if (side === "down") out.top += out.height;
  return out;
}

function tabTarget(id: string, pane: string | null, x: number, y: number): TabDropTarget | null {
  const el = document.elementFromPoint(x, y);
  const strip = el?.closest<HTMLElement>("[data-strip-group]");
  if (strip) {
    const group = strip.dataset.stripGroup!;
    const tabEls = [...strip.querySelectorAll<HTMLElement>("[data-tab-wrap]")];
    let index = tabEls.length;
    let lineX = tabEls.length ? tabEls[tabEls.length - 1].getBoundingClientRect().right : strip.getBoundingClientRect().left + 8;
    for (let i = 0; i < tabEls.length; i++) {
      const r = tabEls[i].getBoundingClientRect();
      if (x < r.left + r.width / 2) {
        index = i;
        lineX = r.left;
        break;
      }
    }
    return { kind: "strip", group, index, x: lineX };
  }
  const panel = el?.closest<HTMLElement>("[data-group-panel]");
  const group = panel?.dataset.groupPanel;
  if (!panel || !group) return null;
  const r = panel.getBoundingClientRect();
  const own = tabs.groupOf(id) === group;
  // A terminal can join the terminal shown there (a tab not into itself).
  const shown = tabs.get(tabs.group(group)?.activeId ?? "");
  const joinable = canJoin(tabs.get(id), shown) && (shown?.id !== id || !!pane);

  // Near the group's edge: split the group, if there is room for two.
  const [edge, dist] = nearestEdge(r, x, y);
  const room = edge === "left" || edge === "right" ? r.width / 2 >= GROUP_MIN_W : r.height / 2 >= GROUP_MIN_H;
  // A tab alone in its group can't split it (a pane can: it leaves as a new tab).
  if (dist < (joinable ? EDGE_JOINABLE : EDGE) && room && (!own || !!pane || tabs.canSplit(id))) {
    return { kind: "group", group, edge, rect: half(r, edge) };
  }
  // Near a pane's edge: join next to it.
  const paneEl = joinable ? el?.closest<HTMLElement>("[data-terminal-pane]") : null;
  const target = paneEl?.dataset.terminalPane;
  if (paneEl && target && target !== pane && shown) {
    const pr = paneEl.getBoundingClientRect();
    const [side, d] = nearestEdge(pr, x, y);
    if (d < PANE_EDGE) return { kind: "pane", tab: shown.id, pane: target, edge: side, rect: half(pr, side) };
  }
  // The middle: into the group.
  if (own) return null;
  return { kind: "group", group, edge: null, rect: { left: r.left, top: r.top, width: r.width, height: r.height } };
}

export const dnd = new Dnd();

// ---------------------------------------------------------------- OS drops

async function uploadPaths(paths: string[], target: DropTarget) {
  if (!paths.length) return;
  await transfers.start({
    sources: paths.map((path) => ({ hostId: null, path })),
    destination: { hostId: target.hostId, path: target.dir },
  });
}

/** Listens for files dragged in from the OS. */
export async function installOsDrop(): Promise<Unlisten> {
  if (isTauri) {
    const { getCurrentWebview } = await import("@tauri-apps/api/webview");
    return getCurrentWebview().onDragDropEvent((event) => {
      const p = event.payload;
      if (p.type === "leave") {
        dnd.os = null;
        return;
      }
      // Positions are physical pixels relative to the webview.
      const x = p.position.x / window.devicePixelRatio;
      const y = p.position.y / window.devicePixelRatio;
      const target = hitTest(x, y);
      const own = dnd.native;
      if (p.type === "enter") dnd.os = { x, y, count: own ? own.drag.entries.length : p.paths.length, target, own: !!own };
      else if (p.type === "over") dnd.os = { x, y, count: dnd.os?.count ?? 0, target, own: !!own };
      else {
        dnd.os = null;
        if (own) {
          // Our drag-out came back: copy the original entries, not the temporary copy.
          const drag: FileDrag = { ...own.drag, x, y, target, copy: true, outside: false };
          if (dropAllowed(drag)) own.onDrop(drag);
        } else if (target) void uploadPaths(p.paths, target);
        else if (p.paths.length) toasts.info("Drop onto a file pane", "Open a files tab and drop onto it to upload.");
      }
    });
  }

  // Browser preview: HTML5 file drags have no paths, so only show the target.
  const hasFiles = (e: DragEvent) => !!e.dataTransfer && [...e.dataTransfer.types].includes("Files");
  const over = (e: DragEvent) => {
    if (!hasFiles(e)) return;
    e.preventDefault();
    const target = hitTest(e.clientX, e.clientY);
    if (e.dataTransfer) e.dataTransfer.dropEffect = target ? "copy" : "none";
    dnd.os = { x: e.clientX, y: e.clientY, count: e.dataTransfer?.items.length ?? 0, target, own: false };
  };
  const leave = (e: DragEvent) => {
    if (!e.relatedTarget) dnd.os = null;
  };
  const drop = (e: DragEvent) => {
    if (!hasFiles(e)) return;
    e.preventDefault();
    const target = dnd.os?.target;
    dnd.os = null;
    if (target) {
      toasts.info(
        "Drop files in the desktop app",
        "Dropping files from your computer uploads them in the Patchbay desktop app. The browser preview can't read file paths.",
      );
    }
  };
  window.addEventListener("dragover", over);
  window.addEventListener("dragleave", leave);
  window.addEventListener("drop", drop);
  return () => {
    window.removeEventListener("dragover", over);
    window.removeEventListener("dragleave", leave);
    window.removeEventListener("drop", drop);
  };
}
