// Session restore: the open tabs, their groups (split view) and per-tab
// state are saved to localStorage as they change and brought back on launch.
//
// Storage: key `patchbay.session.v1`, JSON `{ version: 1, savedAt, tabs, groups,
// focused, splitRatio }`. Tabs are stored as their (already serializable)
// descriptors — files tabs keep their path, terminal tabs their pane tree with
// each pane's last known directory (OSC 7). Everything is validated on load;
// unknown kinds, tabs of deleted hosts and malformed entries are dropped. A
// future format bumps `version` and migrates in `parse`.
//
// Restored tabs start dormant (tabs.svelte.ts): nothing connects and no shell
// starts until the tab is first shown, at which point remote files/terminal
// tabs connect their host. With `reconnect: "eager"` every restored tab is
// woken at launch and all their hosts connect right away. Terminals come back
// as fresh shells in the remembered directory; scrollback isn't restored.

import { untrack } from "svelte";
import { leaves, type PaneNode } from "../terminal/layout";
import type { HostId } from "../types";
import { connections } from "./connections.svelte";
import { hosts } from "./hosts.svelte";
import { settings } from "./settings.svelte";
import { SPLIT_MAX, SPLIT_MIN, tabs, type Group, type Tab } from "./tabs.svelte";

const KEY = "patchbay.session.v1";
const VERSION = 1;
const SAVE_DELAY = 400;
const MAX_TABS = 200;
const MAX_DEPTH = 16;

interface SavedGroup {
  tabIds: string[];
  activeId: string | null;
}

interface SavedSession {
  version: typeof VERSION;
  savedAt: number;
  tabs: Tab[];
  groups: SavedGroup[];
  focused: number;
  splitRatio: number;
}

// ------------------------------------------------------------- serialize

function snapshot(): SavedSession {
  return {
    version: VERSION,
    savedAt: Date.now(),
    tabs: $state.snapshot(tabs.list) as Tab[],
    groups: tabs.groups.map((g) => ({ tabIds: [...g.tabIds], activeId: g.activeId })),
    focused: tabs.focused,
    splitRatio: tabs.splitRatio,
  };
}

// ----------------------------------------------------------------- parse

type Obj = Record<string, unknown>;
const isObj = (v: unknown): v is Obj => !!v && typeof v === "object" && !Array.isArray(v);
const str = (v: unknown): string | null => (typeof v === "string" && v.length > 0 && v.length < 4096 ? v : null);
const optStr = (v: unknown): string | null => (typeof v === "string" && v.length < 4096 ? v : null);

function parseLayout(raw: unknown, depth = 0): PaneNode | null {
  if (!isObj(raw) || depth > MAX_DEPTH) return null;
  const id = str(raw.id);
  if (!id) return null;
  if (raw.type === "pane") return { type: "pane", id, cwd: optStr(raw.cwd) };
  if (raw.type !== "split" || (raw.dir !== "row" && raw.dir !== "column")) return null;
  const a = parseLayout(raw.a, depth + 1);
  const b = parseLayout(raw.b, depth + 1);
  if (!a || !b) return a ?? b;
  const ratio = typeof raw.ratio === "number" && Number.isFinite(raw.ratio) ? Math.min(0.9, Math.max(0.1, raw.ratio)) : 0.5;
  return { type: "split", id, dir: raw.dir, ratio, a, b };
}

function parseTab(raw: unknown, hostExists: (id: HostId) => boolean): Tab | null {
  if (!isObj(raw)) return null;
  const id = str(raw.id);
  if (!id) return null;
  const hostId = raw.hostId === null ? null : str(raw.hostId);
  if (hostId === null && raw.hostId !== null) return null;
  if (hostId !== null && !hostExists(hostId)) return null;
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
function parse(raw: unknown, hostExists: (id: HostId) => boolean) {
  if (!isObj(raw) || raw.version !== VERSION || !Array.isArray(raw.tabs) || !Array.isArray(raw.groups)) return null;
  const list: Tab[] = [];
  const seen = new Set<string>();
  const singletons = new Set<string>();
  for (const t of raw.tabs.slice(0, MAX_TABS)) {
    const tab = parseTab(t, hostExists);
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
  const placed = new Set<string>();
  const groups: Group[] = [];
  for (const g of raw.groups.slice(0, 2)) {
    if (!isObj(g) || !Array.isArray(g.tabIds)) continue;
    const tabIds = g.tabIds.filter((id): id is string => typeof id === "string" && seen.has(id) && !placed.has(id));
    tabIds.forEach((id) => placed.add(id));
    if (!tabIds.length) continue;
    const activeId = typeof g.activeId === "string" && tabIds.includes(g.activeId) ? g.activeId : tabIds[0];
    groups.push({ id: `group-restored-${groups.length}`, tabIds, activeId });
  }
  // Tabs missing from every group go to the first one.
  const orphans = list.filter((t) => !placed.has(t.id)).map((t) => t.id);
  if (orphans.length) {
    if (groups.length) groups[0].tabIds.push(...orphans);
    else groups.push({ id: "group-restored-0", tabIds: orphans, activeId: orphans[0] });
  }
  if (!groups.length) return null;
  const inGroups = new Set(groups.flatMap((g) => g.tabIds));
  const ratio = typeof raw.splitRatio === "number" && Number.isFinite(raw.splitRatio) ? raw.splitRatio : 0.5;
  return {
    list: list.filter((t) => inGroups.has(t.id)),
    groups,
    focused: typeof raw.focused === "number" && raw.focused < groups.length ? Math.max(0, Math.floor(raw.focused)) : 0,
    splitRatio: Math.min(SPLIT_MAX, Math.max(SPLIT_MIN, ratio)),
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
      const state = parse(raw, (id) => hosts.byId.has(id));
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
        void tabs.focused;
        void tabs.splitRatio;
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
