// Session restore: the open tabs, their groups (split view) and per-tab
// state are saved to localStorage as they change and brought back on launch.
//
// Storage: key `patchbay.session.v1`, JSON `{ version: 2, savedAt, tabs, groups,
// layout, focused }` (`groups` with their tab order, `layout` the split tree of
// group ids, `focused` a group id). Version 1 had at most two groups side by
// side (`focused` an index, `splitRatio`) and is migrated in `parse`. Tabs are
// stored as their (already serializable) descriptors — files tabs keep their
// path, terminal tabs their pane tree with each pane's last known directory
// (OSC 7). Everything is validated on load; unknown kinds, tabs of deleted
// hosts and malformed entries are dropped.
//
// Restored tabs start dormant (tabs.svelte.ts): nothing connects and no shell
// starts until the tab is first shown, at which point remote files/terminal
// tabs connect their host. With `reconnect: "eager"` every restored tab is
// woken at launch and all their hosts connect right away. Terminals come back
// as fresh shells in the remembered directory; scrollback isn't restored.

import { untrack } from "svelte";
import { leaves, type PaneNode } from "../terminal/layout";
import type { HostId } from "../types";
import { clampRatio, type Tree } from "../util/splitTree";
import { connections } from "./connections.svelte";
import { hosts } from "./hosts.svelte";
import { settings } from "./settings.svelte";
import { tabs, type Group, type GroupNode, type Tab } from "./tabs.svelte";

const KEY = "patchbay.session.v1";
const VERSION = 2;
const SAVE_DELAY = 400;
const MAX_TABS = 200;
const MAX_DEPTH = 16;
const MAX_GROUPS = 32;

interface SavedGroup {
  id: string;
  tabIds: string[];
  activeId: string | null;
  previewId: string | null;
}

interface SavedSession {
  version: typeof VERSION;
  savedAt: number;
  tabs: Tab[];
  groups: SavedGroup[];
  layout: GroupNode;
  focused: string;
}

// ------------------------------------------------------------- serialize

function snapshot(): SavedSession {
  return {
    version: VERSION,
    savedAt: Date.now(),
    tabs: $state.snapshot(tabs.list) as Tab[],
    groups: tabs.groups.map((g) => ({ id: g.id, tabIds: [...g.tabIds], activeId: g.activeId, previewId: g.previewId })),
    layout: $state.snapshot(tabs.layout) as GroupNode,
    focused: tabs.focusedGroup,
  };
}

// ----------------------------------------------------------------- parse

type Obj = Record<string, unknown>;
const isObj = (v: unknown): v is Obj => !!v && typeof v === "object" && !Array.isArray(v);
const str = (v: unknown): string | null => (typeof v === "string" && v.length > 0 && v.length < 4096 ? v : null);
const optStr = (v: unknown): string | null => (typeof v === "string" && v.length < 4096 ? v : null);

/** Parses a split tree, with `leaf` parsing (or dropping) its leaves. */
function parseTree<L extends { type: string; id: string }>(raw: unknown, leaf: (raw: Obj, id: string) => L | null, depth = 0): Tree<L> | null {
  if (!isObj(raw) || depth > MAX_DEPTH) return null;
  const id = str(raw.id);
  if (!id) return null;
  if (raw.type !== "split") return leaf(raw, id);
  if (raw.dir !== "row" && raw.dir !== "column") return null;
  const a = parseTree(raw.a, leaf, depth + 1);
  const b = parseTree(raw.b, leaf, depth + 1);
  if (!a || !b) return a ?? b;
  const ratio = typeof raw.ratio === "number" && Number.isFinite(raw.ratio) ? clampRatio(raw.ratio) : 0.5;
  return { type: "split", id, dir: raw.dir, ratio, a, b };
}

function parseLayout(raw: unknown): PaneNode | null {
  return parseTree(raw, (r, id) => (r.type === "pane" ? { type: "pane" as const, id, cwd: optStr(r.cwd) } : null));
}

/** `currentId` maps a saved host id to the host's current id, or null if it is gone. */
function parseTab(raw: unknown, currentId: (id: HostId) => HostId | null): Tab | null {
  if (!isObj(raw)) return null;
  const id = str(raw.id);
  if (!id) return null;
  const savedId = raw.hostId === null ? null : str(raw.hostId);
  if (savedId === null && raw.hostId !== null) return null;
  const hostId = savedId === null ? null : currentId(savedId);
  if (savedId !== null && hostId === null) return null;
  switch (raw.kind) {
    case "host":
      return hostId ? { id, kind: "host", hostId } : null;
    case "files":
      return { id, kind: "files", hostId, path: optStr(raw.path) || null };
    case "forwards":
      return { id, kind: "forwards", hostId };
    case "terminal": {
      const layout = parseLayout(raw.layout);
      if (!layout) return null;
      const ids = new Set(leaves(layout).map((l) => l.id));
      const focused = str(raw.focusedPane);
      const zoomed = str(raw.zoomedPane);
      return {
        id,
        kind: "terminal",
        hostId,
        cwd: optStr(raw.cwd) || null,
        layout,
        focusedPane: focused && ids.has(focused) ? focused : leaves(layout)[0].id,
        zoomedPane: zoomed && ids.has(zoomed) && ids.size > 1 ? zoomed : null,
      };
    }
    default:
      return null;
  }
}

/** Validates stored data into a consistent layout, or null if there's nothing to restore. */
function parse(raw: unknown, currentId: (id: HostId) => HostId | null) {
  if (!isObj(raw) || (raw.version !== 1 && raw.version !== VERSION) || !Array.isArray(raw.tabs) || !Array.isArray(raw.groups)) {
    return null;
  }
  const list: Tab[] = [];
  const seen = new Set<string>();
  const singletons = new Set<string>();
  for (const t of raw.tabs.slice(0, MAX_TABS)) {
    const tab = parseTab(t, currentId);
    if (!tab || seen.has(tab.id)) continue;
    // Host overviews and forwards views exist once per host.
    if (tab.kind === "host" || tab.kind === "forwards") {
      const key = `${tab.kind}:${tab.hostId}`;
      if (singletons.has(key)) continue;
      singletons.add(key);
    }
    seen.add(tab.id);
    list.push(tab);
  }
  const v1 = raw.version === 1;
  const placed = new Set<string>();
  const groups: Group[] = [];
  for (const [i, g] of raw.groups.slice(0, v1 ? 2 : MAX_GROUPS).entries()) {
    if (!isObj(g) || !Array.isArray(g.tabIds)) continue;
    const id = (!v1 && str(g.id)) || `group-restored-${i}`;
    if (groups.some((x) => x.id === id)) continue;
    const tabIds = g.tabIds.filter((t): t is string => typeof t === "string" && seen.has(t) && !placed.has(t));
    tabIds.forEach((t) => placed.add(t));
    if (!tabIds.length) continue;
    const activeId = typeof g.activeId === "string" && tabIds.includes(g.activeId) ? g.activeId : tabIds[0];
    const previewId = typeof g.previewId === "string" && tabIds.includes(g.previewId) ? g.previewId : null;
    groups.push({ id, tabIds, activeId, previewId });
  }
  // Tabs missing from every group go to the first one.
  const orphans = list.filter((t) => !placed.has(t.id)).map((t) => t.id);
  if (orphans.length) {
    if (groups.length) groups[0].tabIds.push(...orphans);
    else groups.push({ id: "group-restored-0", tabIds: orphans, activeId: orphans[0], previewId: null });
  }
  if (!groups.length) return null;
  const ids = new Set(groups.map((g) => g.id));

  let layout: GroupNode | null;
  let focused: string | undefined;
  if (v1) {
    // At most two groups side by side.
    const ratio = typeof raw.splitRatio === "number" && Number.isFinite(raw.splitRatio) ? raw.splitRatio : 0.5;
    const [a, b] = groups.map((g) => ({ type: "group" as const, id: g.id }));
    layout = b ? { type: "split", id: "split-restored", dir: "row", ratio: clampRatio(ratio), a, b } : a;
    focused = typeof raw.focused === "number" ? groups[raw.focused]?.id : undefined;
  } else {
    // Every group appears once in the tree; others are dropped, missing ones added on the right.
    const inTree = new Set<string>();
    const leaf = (_: Obj, id: string) => (ids.has(id) && !inTree.has(id) ? (inTree.add(id), { type: "group" as const, id }) : null);
    layout = parseTree(raw.layout, leaf);
    for (const g of groups) {
      if (inTree.has(g.id)) continue;
      const added = { type: "group" as const, id: g.id };
      layout = layout ? { type: "split", id: `split-restored-${g.id}`, dir: "row", ratio: 0.5, a: layout, b: added } : added;
    }
    focused = str(raw.focused) ?? undefined;
  }
  if (!layout) return null;
  const inGroups = new Set(groups.flatMap((g) => g.tabIds));
  return {
    list: list.filter((t) => inGroups.has(t.id)),
    groups,
    layout,
    focusedGroup: focused && ids.has(focused) ? focused : groups[0].id,
  };
}

// ---------------------------------------------------------------- store

/** Tabs whose view needs the host connected to show anything. */
const needsConnection = (t: Tab): t is Tab & { hostId: HostId } =>
  t.hostId !== null && (t.kind === "files" || t.kind === "terminal");

function connectIdle(hostId: HostId) {
  if (connections.get(hostId).phase === "idle") void connections.connect(hostId);
}

class Session {
  /** True until startup decided whether tabs are restored (avoids a Welcome flash). */
  pending = $state(true);
  /** Number of tabs brought back at launch (0 = nothing restored). */
  restoredCount = $state(0);

  #timer: ReturnType<typeof setTimeout> | undefined;

  /** Restores the saved session if enabled. Call once hosts are loaded. */
  restore(): boolean {
    try {
      if (!settings.session.restoreTabs || tabs.list.length > 0) return false;
      let raw: unknown;
      try {
        raw = JSON.parse(localStorage.getItem(KEY) ?? "null");
      } catch {
        return false;
      }
      const state = parse(raw, (id) => hosts.currentId(id));
      if (!state || !state.list.length) return false;
      const eager = settings.session.reconnect === "eager";
      tabs.replaceAll({ ...state, dormant: eager ? [] : state.list.map((t) => t.id) });
      this.restoredCount = state.list.length;
      if (eager) {
        const ids = new Set(state.list.filter(needsConnection).map((t) => t.hostId));
        for (const id of ids) connectIdle(id);
      }
      return true;
    } finally {
      this.pending = false;
    }
  }

  /**
   * Starts saving the layout as it changes and waking restored tabs when
   * they're shown. Returns a cleanup function.
   */
  start(): () => void {
    const flush = () => this.#save();
    window.addEventListener("pagehide", flush);
    const stop = $effect.root(() => {
      // Save (debounced) on any change to tabs, groups or per-tab state.
      $effect(() => {
        void JSON.stringify(tabs.list);
        void JSON.stringify(tabs.groups);
        void JSON.stringify(tabs.layout);
        void tabs.focusedGroup;
        untrack(() => {
          clearTimeout(this.#timer);
          this.#timer = setTimeout(() => this.#save(), SAVE_DELAY);
        });
      });
      // Lazy reconnect: a dormant tab wakes (and connects) when first shown.
      $effect(() => {
        const shown = tabs.visible.filter((t) => tabs.isDormant(t.id));
        if (!shown.length) return;
        untrack(() => {
          for (const t of shown) {
            tabs.wake(t.id);
            if (needsConnection(t)) connectIdle(t.hostId);
          }
        });
      });
    });
    return () => {
      stop();
      window.removeEventListener("pagehide", flush);
      this.#save();
    };
  }

  #save() {
    clearTimeout(this.#timer);
    this.#timer = undefined;
    try {
      localStorage.setItem(KEY, JSON.stringify(snapshot()));
    } catch {
      // Storage full or unavailable: the session just isn't remembered.
    }
  }
}

export const session = new Session();
