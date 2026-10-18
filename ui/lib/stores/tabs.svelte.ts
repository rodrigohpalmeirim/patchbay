// Tab model for the main area.
//
// A tab is a typed, serializable descriptor (`kind` + per-kind params). The
// view for each kind is looked up in `views/tabKinds.ts`, so new tab types are
// added by (1) extending the `Tab` union here and (2) registering a view.
//
// `hostId: null` means "this computer" (local files and shells); for the
// forwards kind it means "all hosts".
//
// Split view: tabs live in one or two *groups* shown side by side, each with
// its own strip and active tab. `list` holds every tab (lookup + render
// order), `groups` holds the per-group ordering. Panels for all tabs are
// rendered in one keyed list and only positioned per group, so moving a tab
// between groups never remounts its view (terminals etc. survive).
// Tabs are all kept mounted (inactive ones are hidden).
//
// Session restore (stores/session.svelte.ts) brings tabs back *dormant*:
// views skip connecting / starting shells until the tab is first shown
// (`wake`), unless the user chose to reconnect everything on launch.

import type { PaneNode } from "../terminal/layout";
import type { HostId } from "../types";

interface TabBase {
  id: string;
  /** Host the tab belongs to; null = this computer. */
  hostId: HostId | null;
}

export interface HostTab extends TabBase {
  kind: "host";
  hostId: HostId;
}

export interface FilesTab extends TabBase {
  kind: "files";
  /** Current directory; null until the home directory is resolved. */
  path: string | null;
}

/**
 * Terminal tab: a tree of split panes, one shell each. Live sessions (xterm,
 * backend terminal ids) are kept in stores/terminals.svelte.ts, keyed by pane id.
 */
export interface TerminalTab extends TabBase {
  kind: "terminal";
  /** Directory the tab was opened in (null = home). */
  cwd: string | null;
  layout: PaneNode;
  /** Pane that receives keyboard input, snippets and pane actions. */
  focusedPane: string;
  /** Pane shown maximized, if any. */
  zoomedPane: string | null;
}

/** Port forward rules of one host, or of all hosts when hostId is null. */
export interface ForwardsTab extends TabBase {
  kind: "forwards";
}

export type Tab = HostTab | FilesTab | TerminalTab | ForwardsTab;
export type TabKind = Tab["kind"];

export interface Group {
  id: string;
  tabIds: string[];
  activeId: string | null;
}

/** Tab kinds that may only be open once per host (re-opening focuses it). */
const SINGLETON: ReadonlySet<TabKind> = new Set<TabKind>(["host", "forwards"]);

let seq = 0;
const newId = (prefix: string) => `${prefix}-${Date.now().toString(36)}-${(seq++).toString(36)}`;

type TabSpec = Tab extends infer T ? (T extends Tab ? Omit<T, "id"> : never) : never;

export interface OpenOptions {
  activate?: boolean;
  /** Target group: an index, "other" (creates the split if needed) or the focused one. */
  group?: number | "other";
}

export const SPLIT_MIN = 0.2;
export const SPLIT_MAX = 0.8;

class Tabs {
  list = $state<Tab[]>([]);
  groups = $state<Group[]>([{ id: newId("group"), tabIds: [], activeId: null }]);
  /** Index of the group that has keyboard focus / receives new tabs. */
  focused = $state(0);
  /** Width fraction of the left group when split. */
  splitRatio = $state(0.5);
  /** Restored tabs that haven't been shown yet (see the header). */
  dormant = $state<Record<string, true>>({});

  byId = $derived(new Map(this.list.map((t) => [t.id, t])));
  isSplit = $derived(this.groups.length > 1);
  activeId = $derived<string | null>(this.groups[this.focused]?.activeId ?? null);
  active = $derived<Tab | null>((this.activeId && this.byId.get(this.activeId)) || null);
  activeHostId = $derived<HostId | null>(this.active?.hostId ?? null);
  /** Active tab of every group. */
  visible = $derived(
    this.groups.map((g) => (g.activeId ? this.byId.get(g.activeId) : undefined)).filter((t): t is Tab => !!t),
  );

  get(id: string): Tab | undefined {
    return this.byId.get(id);
  }

  groupOf(id: string): number {
    return this.groups.findIndex((g) => g.tabIds.includes(id));
  }

  tabsIn(group: number): Tab[] {
    return (this.groups[group]?.tabIds ?? []).map((id) => this.byId.get(id)).filter((t): t is Tab => !!t);
  }

  isVisible(id: string): boolean {
    return this.groups.some((g) => g.activeId === id);
  }

  open(spec: TabSpec, { activate = true, group }: OpenOptions = {}): Tab {
    let tab = SINGLETON.has(spec.kind)
      ? this.list.find((t) => t.kind === spec.kind && t.hostId === spec.hostId)
      : undefined;
    if (tab) {
      if (group !== undefined) this.moveToGroup(tab.id, this.#resolveGroup(group));
    } else {
      tab = { ...spec, id: newId(spec.kind) } as Tab;
      const gi = group === undefined ? this.focused : this.#resolveGroup(group);
      this.list = [...this.list, tab];
      const g = this.groups[gi];
      // Insert after the group's active tab, like browsers do.
      const at = g.activeId ? g.tabIds.indexOf(g.activeId) : -1;
      g.tabIds.splice(at === -1 ? g.tabIds.length : at + 1, 0, tab.id);
      if (!activate && !g.activeId) g.activeId = tab.id;
    }
    if (activate) this.activate(tab.id);
    return tab;
  }

  /** Index for "other"/explicit groups, creating the second group if needed. */
  #resolveGroup(group: number | "other"): number {
    if (group === "other") {
      if (this.groups.length === 1) {
        this.groups.push({ id: newId("group"), tabIds: [], activeId: null });
        return 1;
      }
      return this.focused === 0 ? 1 : 0;
    }
    if (group >= this.groups.length) {
      this.groups.push({ id: newId("group"), tabIds: [], activeId: null });
      return this.groups.length - 1;
    }
    return Math.max(0, group);
  }

  openHost(hostId: HostId) {
    return this.open({ kind: "host", hostId });
  }

  /** Forwards of one host, or all hosts (null). */
  openForwards(hostId: HostId | null, opts: OpenOptions = {}) {
    return this.open({ kind: "forwards", hostId }, opts);
  }

  isDormant(id: string): boolean {
    return this.dormant[id] === true;
  }

  /** Marks a restored tab as shown; its view may now connect / start shells. */
  wake(id: string) {
    if (this.dormant[id]) delete this.dormant[id];
  }

  /** Replaces the whole layout (session restore). Ids must be consistent. */
  replaceAll(state: { list: Tab[]; groups: Group[]; focused: number; splitRatio: number; dormant: string[] }) {
    this.list = state.list;
    this.groups = state.groups.length ? state.groups : [{ id: newId("group"), tabIds: [], activeId: null }];
    this.focused = Math.min(Math.max(0, state.focused), this.groups.length - 1);
    this.splitRatio = Math.min(SPLIT_MAX, Math.max(SPLIT_MIN, state.splitRatio));
    this.dormant = Object.fromEntries(state.dormant.map((id) => [id, true as const]));
  }

  activate(id: string) {
    const gi = this.groupOf(id);
    if (gi === -1) return;
    this.groups[gi].activeId = id;
    this.focused = gi;
  }

  focusGroup(index: number) {
    if (index >= 0 && index < this.groups.length) this.focused = index;
  }

  /** Focus the other side of the split. Returns false if not split. */
  focusOther(): boolean {
    if (!this.isSplit) return false;
    this.focused = this.focused === 0 ? 1 : 0;
    return true;
  }

  close(id: string) {
    const gi = this.groupOf(id);
    if (gi === -1) return;
    const g = this.groups[gi];
    const i = g.tabIds.indexOf(id);
    g.tabIds.splice(i, 1);
    if (g.activeId === id) g.activeId = g.tabIds[i] ?? g.tabIds[i - 1] ?? null;
    this.list = this.list.filter((t) => t.id !== id);
    this.wake(id);
    this.#collapseEmpty();
  }

  /** Drops an empty group so the split collapses. */
  #collapseEmpty() {
    if (this.groups.length < 2) return;
    const empty = this.groups.findIndex((g) => g.tabIds.length === 0);
    if (empty === -1) return;
    this.groups.splice(empty, 1);
    this.focused = 0;
  }

  closeActive() {
    if (this.activeId) this.close(this.activeId);
  }

  closeOthers(id: string) {
    const gi = this.groupOf(id);
    if (gi === -1) return;
    for (const other of this.groups[gi].tabIds.filter((t) => t !== id)) this.close(other);
  }

  closeForHost(hostId: HostId) {
    for (const t of this.list.filter((t) => t.hostId === hostId)) this.close(t.id);
  }

  /** Moves tabs of renamed hosts to the hosts' new ids. */
  retarget(currentId: (id: HostId) => HostId | null) {
    for (const t of this.list) {
      const id = t.hostId === null ? null : currentId(t.hostId);
      if (id && id !== t.hostId) t.hostId = id;
    }
  }

  /** Closes tabs whose host no longer exists (local tabs are kept). */
  prune(exists: (id: HostId) => boolean) {
    for (const t of this.list.filter((t) => t.hostId !== null && !exists(t.hostId))) this.close(t.id);
  }

  /** Cycles through the focused group's tabs. */
  cycle(delta: number) {
    const g = this.groups[this.focused];
    if (!g || g.tabIds.length < 2) return;
    const i = g.activeId ? g.tabIds.indexOf(g.activeId) : 0;
    const n = g.tabIds.length;
    g.activeId = g.tabIds[(((i + delta) % n) + n) % n];
  }

  activateIndex(index: number) {
    const g = this.groups[this.focused];
    const id = g?.tabIds[index === -1 ? g.tabIds.length - 1 : index];
    if (id) this.activate(id);
  }

  /** Moves a tab to `toIndex` within `group` (defaults to its own group). */
  move(id: string, toIndex: number, group?: number) {
    const from = this.groupOf(id);
    if (from === -1) return;
    const to = group === undefined ? from : this.#resolveGroup(group);
    const src = this.groups[from];
    const i = src.tabIds.indexOf(id);
    src.tabIds.splice(i, 1);
    if (src.activeId === id) src.activeId = src.tabIds[i] ?? src.tabIds[i - 1] ?? null;
    const dst = this.groups[to];
    const at = from === to && i < toIndex ? toIndex - 1 : toIndex;
    dst.tabIds.splice(Math.max(0, Math.min(at, dst.tabIds.length)), 0, id);
    dst.activeId = id;
    this.focused = to;
    const dstId = dst.id;
    this.#collapseEmpty();
    this.focused = Math.max(0, this.groups.findIndex((g) => g.id === dstId));
  }

  moveToGroup(id: string, group: number | "other") {
    const from = this.groupOf(id);
    if (from === -1) return;
    // Moving the only tab of the only group into a new group is a no-op.
    if (this.groups.length === 1 && this.groups[0].tabIds.length === 1 && group !== 0) return;
    const to = group === "other" ? (from === 0 ? 1 : 0) : group;
    if (to === from) return;
    this.move(id, this.groups[to]?.tabIds.length ?? 0, to);
  }

  /** Can this tab be moved to the other side (needs a tab left behind)? */
  canMoveToOther(): boolean {
    return this.isSplit || this.list.length > 1;
  }

  setSplitRatio(r: number) {
    this.splitRatio = Math.min(SPLIT_MAX, Math.max(SPLIT_MIN, r));
  }

  /** Updates per-kind params (e.g. a files tab's path) in place. */
  update<T extends Tab>(id: string, patch: Partial<Omit<T, "id" | "kind">>) {
    const t = this.byId.get(id);
    if (t) Object.assign(t, patch);
  }

  /** Is the overview of this host visible in any group? */
  isHostVisible(hostId: HostId): boolean {
    return this.visible.some((t) => t.kind === "host" && t.hostId === hostId);
  }
}

export const tabs = new Tabs();
