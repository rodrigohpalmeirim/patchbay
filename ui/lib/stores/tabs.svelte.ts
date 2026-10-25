// Tab model for the main area.
//
// A tab is a typed, serializable descriptor (`kind` + per-kind params). The
// view for each kind is looked up in `views/tabKinds.ts`, so new tab types are
// added by (1) extending the `Tab` union here and (2) registering a view.
//
// `hostId: null` means "this computer" (local files and shells); for the
// forwards kind it means "all hosts".
//
// Split view: tabs live in *groups*, each with its own strip and active tab,
// laid out by a split tree (util/splitTree.ts) whose leaves are the groups.
// `list` holds every tab (lookup + render order), `groups` the per-group
// ordering. Panels for all tabs are rendered in one keyed list and only
// positioned per group, so moving a tab between groups never remounts its
// view (terminals etc. survive). Tabs are all kept mounted (inactive ones are
// hidden). A group that loses its last tab is removed; its neighbour takes
// the space.
//
// Preview tabs: views you pass through (host overviews, port forwards) open
// as a group's *preview* tab, which the next preview in that group replaces,
// so clicking through hosts doesn't pile up tabs. A preview becomes a normal
// tab once it is kept: opened again without `preview`, double-clicked or moved.
//
// Session restore (stores/session.svelte.ts) brings tabs back *dormant*:
// views skip connecting / starting shells until the tab is first shown
// (`wake`), unless the user chose to reconnect everything on launch.
//
// Tabs move between windows with their ids (stores/windows.svelte.ts):
// closed in one window, `insert`ed in the other.

import type { PaneNode } from "../terminal/layout";
import type { HostId } from "../types";
import {
  clampRatio,
  dirAxis,
  findSplit,
  leaves,
  neighbor,
  newId,
  removeLeaf,
  splitLeaf,
  type Direction,
  type Tree,
} from "../util/splitTree";

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
  /** The group's preview tab, if any (see the header). */
  previewId: string | null;
}

export interface GroupLeaf {
  type: "group";
  id: string;
}

export type GroupNode = Tree<GroupLeaf>;

/**
 * Where a tab goes: a group id, "beside" the focused group (a neighbouring
 * group, or a new one on the right), or a new group split off `of` (default:
 * the focused group) on the `split` side.
 */
export type Placement = string | "beside" | { split: Direction; of?: string };

/** Tab kinds that may only be open once per host (re-opening focuses it). */
const SINGLETON: ReadonlySet<TabKind> = new Set<TabKind>(["host", "forwards"]);

type TabSpec = Tab extends infer T ? (T extends Tab ? Omit<T, "id"> : never) : never;

export interface OpenOptions {
  activate?: boolean;
  /** Target group (default: the focused one). */
  group?: Placement;
  /** Open as (or replace) the group's preview tab. */
  preview?: boolean;
}

/** Smallest group size (px) a split may leave. */
export const GROUP_MIN_W = 240;
export const GROUP_MIN_H = 160;

const emptyGroup = (): Group => ({ id: newId("group"), tabIds: [], activeId: null, previewId: null });

class Tabs {
  list = $state<Tab[]>([]);
  groups = $state<Group[]>([emptyGroup()]);
  layout = $state<GroupNode>({ type: "group", id: "" });
  /** Group that has keyboard focus / receives new tabs. */
  focusedGroup = $state("");
  /** Restored tabs that haven't been shown yet (see the header). */
  dormant = $state<Record<string, true>>({});

  constructor() {
    this.layout = { type: "group", id: this.groups[0].id };
    this.focusedGroup = this.groups[0].id;
  }

  byId = $derived(new Map(this.list.map((t) => [t.id, t])));
  #groupById = $derived(new Map(this.groups.map((g) => [g.id, g])));
  isSplit = $derived(this.groups.length > 1);
  /** Group ids in layout order (left to right, top to bottom). */
  order = $derived(leaves(this.layout).map((l) => l.id));
  activeId = $derived<string | null>(this.group(this.focusedGroup)?.activeId ?? null);
  active = $derived<Tab | null>((this.activeId && this.byId.get(this.activeId)) || null);
  activeHostId = $derived<HostId | null>(this.active?.hostId ?? null);
  /** Active tab of every group. */
  visible = $derived(
    this.groups.map((g) => (g.activeId ? this.byId.get(g.activeId) : undefined)).filter((t): t is Tab => !!t),
  );

  get(id: string): Tab | undefined {
    return this.byId.get(id);
  }

  group(id: string): Group | undefined {
    return this.#groupById.get(id);
  }

  /** Id of the group holding a tab. */
  groupOf(id: string): string | undefined {
    return this.groups.find((g) => g.tabIds.includes(id))?.id;
  }

  tabsIn(group: string): Tab[] {
    return (this.group(group)?.tabIds ?? []).map((id) => this.byId.get(id)).filter((t): t is Tab => !!t);
  }

  isVisible(id: string): boolean {
    return this.groups.some((g) => g.activeId === id);
  }

  isPreview(id: string): boolean {
    return this.groups.some((g) => g.previewId === id);
  }

  /** Keeps a preview tab open (it becomes a normal tab). */
  pin(id: string) {
    for (const g of this.groups) if (g.previewId === id) g.previewId = null;
  }

  open(spec: TabSpec, { activate = true, group, preview = false }: OpenOptions = {}): Tab {
    let tab = SINGLETON.has(spec.kind)
      ? this.list.find((t) => t.kind === spec.kind && t.hostId === spec.hostId)
      : undefined;
    if (tab) {
      if (!preview) this.pin(tab.id);
      if (group !== undefined) this.moveToGroup(tab.id, group);
    } else {
      tab = { ...spec, id: newId(spec.kind) } as Tab;
      const g = this.group(group === undefined ? this.focusedGroup : this.#resolve(group)) ?? this.groups[0];
      const replaced = preview && g.previewId ? g.previewId : null;
      this.list = [...this.list, tab];
      // Take the replaced preview's place, else go after the active tab like browsers do.
      const anchor = replaced ?? g.activeId;
      const at = anchor ? g.tabIds.indexOf(anchor) : -1;
      g.tabIds.splice(at === -1 ? g.tabIds.length : replaced ? at : at + 1, 0, tab.id);
      if (preview) g.previewId = tab.id;
      if (!g.activeId || (replaced && g.activeId === replaced)) g.activeId = tab.id;
      if (replaced) this.close(replaced);
    }
    if (activate) this.activate(tab.id);
    return tab;
  }

  /**
   * Adds a tab moved here from another window, keeping its id, and shows it:
   * in `group` (default: the focused one) at `index` (default: after its
   * active tab). A host overview or forwards view already open here is
   * moved there and shown instead. Returns the tab shown.
   */
  insert(tab: Tab, { dormant = false, group, index }: { dormant?: boolean; group?: Placement; index?: number } = {}): Tab {
    const open = SINGLETON.has(tab.kind) && this.list.find((t) => t.kind === tab.kind && t.hostId === tab.hostId);
    if (open) {
      if (group !== undefined && index !== undefined) this.move(open.id, index, group);
      else if (group !== undefined) this.moveToGroup(open.id, group);
      this.activate(open.id);
      return open;
    }
    const g = this.group(group === undefined ? this.focusedGroup : this.#resolve(group)) ?? this.groups[0];
    this.list = [...this.list, tab];
    const at = index ?? (g.activeId ? g.tabIds.indexOf(g.activeId) + 1 : g.tabIds.length);
    g.tabIds.splice(Math.max(0, Math.min(at, g.tabIds.length)), 0, tab.id);
    if (dormant) this.dormant[tab.id] = true;
    this.activate(tab.id);
    return this.get(tab.id) ?? tab;
  }

  /** Group id for a placement, creating a group if needed. */
  #resolve(placement: Placement): string {
    if (placement === "beside") return this.#beside(this.focusedGroup);
    if (typeof placement === "string") return this.group(placement) ? placement : this.focusedGroup;
    return this.#splitGroup(placement.of ?? this.focusedGroup, placement.split);
  }

  /** A group next to `from` (right, left, below, above), or a new one on its right. */
  #beside(from: string): string {
    for (const dir of ["right", "left", "down", "up"] as const) {
      const n = neighbor(this.layout, from, dir);
      if (n) return n;
    }
    return this.#splitGroup(from, "right");
  }

  /** Splits group `of`, putting a new empty group on the `dir` side. */
  #splitGroup(of: string, dir: Direction): string {
    const g = emptyGroup();
    const target = this.group(of) ? of : this.focusedGroup;
    this.groups.push(g);
    this.layout = splitLeaf(this.layout, target, dirAxis(dir), { type: "group", id: g.id }, dir === "left" || dir === "up");
    return g.id;
  }

  openHost(hostId: HostId, opts: OpenOptions = {}) {
    return this.open({ kind: "host", hostId }, { preview: true, ...opts });
  }

  /** Forwards of one host, or all hosts (null). */
  openForwards(hostId: HostId | null, opts: OpenOptions = {}) {
    return this.open({ kind: "forwards", hostId }, { preview: true, ...opts });
  }

  isDormant(id: string): boolean {
    return this.dormant[id] === true;
  }

  /** Marks a restored tab as shown; its view may now connect / start shells. */
  wake(id: string) {
    if (this.dormant[id]) delete this.dormant[id];
  }

  /** Replaces the whole layout (session restore). Ids must be consistent. */
  replaceAll(state: { list: Tab[]; groups: Group[]; layout: GroupNode; focusedGroup: string; dormant: string[] }) {
    this.list = state.list;
    this.groups = state.groups;
    this.layout = state.layout;
    this.focusedGroup = state.groups.some((g) => g.id === state.focusedGroup) ? state.focusedGroup : state.groups[0].id;
    this.dormant = Object.fromEntries(state.dormant.map((id) => [id, true as const]));
  }

  activate(id: string) {
    const g = this.groups.find((g) => g.tabIds.includes(id));
    if (!g) return;
    g.activeId = id;
    this.focusedGroup = g.id;
  }

  focusGroup(id: string | undefined) {
    if (id && this.group(id)) this.focusedGroup = id;
  }

  /** Focuses the group next to the focused one in `dir`. Returns false if there is none. */
  focusDir(dir: Direction): boolean {
    const n = neighbor(this.layout, this.focusedGroup, dir);
    if (n) this.focusedGroup = n;
    return !!n;
  }

  /** Focuses the next (or previous) group in layout order. Returns false if not split. */
  cycleGroup(delta: number): boolean {
    const n = this.order.length;
    if (n < 2) return false;
    const i = this.order.indexOf(this.focusedGroup);
    this.focusedGroup = this.order[(((i + delta) % n) + n) % n];
    return true;
  }

  close(id: string) {
    const g = this.groups.find((g) => g.tabIds.includes(id));
    if (!g) return;
    const i = g.tabIds.indexOf(id);
    g.tabIds.splice(i, 1);
    if (g.activeId === id) g.activeId = g.tabIds[i] ?? g.tabIds[i - 1] ?? null;
    if (g.previewId === id) g.previewId = null;
    this.list = this.list.filter((t) => t.id !== id);
    this.wake(id);
    this.#collapse(g.id);
  }

  /** Drops group `id` if it is empty (and not the only one); a neighbour takes its space. */
  #collapse(id: string) {
    const g = this.group(id);
    if (this.groups.length < 2 || !g || g.tabIds.length) return;
    if (this.focusedGroup === id) {
      const order = this.order;
      const i = order.indexOf(id);
      this.focusedGroup = order[i + 1] ?? order[i - 1];
    }
    this.layout = removeLeaf(this.layout, id) ?? { type: "group", id: this.focusedGroup };
    this.groups = this.groups.filter((x) => x.id !== id);
  }

  closeActive() {
    if (this.activeId) this.close(this.activeId);
  }

  closeOthers(id: string) {
    const g = this.groups.find((g) => g.tabIds.includes(id));
    if (!g) return;
    for (const other of g.tabIds.filter((t) => t !== id)) this.close(other);
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
    const g = this.group(this.focusedGroup);
    if (!g || g.tabIds.length < 2) return;
    const i = g.activeId ? g.tabIds.indexOf(g.activeId) : 0;
    const n = g.tabIds.length;
    g.activeId = g.tabIds[(((i + delta) % n) + n) % n];
  }

  activateIndex(index: number) {
    const g = this.group(this.focusedGroup);
    const id = g?.tabIds[index === -1 ? g.tabIds.length - 1 : index];
    if (id) this.activate(id);
  }

  /** Moves a tab to `toIndex` within `group` (defaults to its own group). Keeps a preview open. */
  move(id: string, toIndex: number, group?: Placement) {
    const src = this.groups.find((g) => g.tabIds.includes(id));
    if (!src) return;
    const to = group === undefined ? src.id : this.#resolve(group);
    const dst = this.group(to);
    if (!dst) return;
    this.pin(id);
    const i = src.tabIds.indexOf(id);
    src.tabIds.splice(i, 1);
    if (src.activeId === id) src.activeId = src.tabIds[i] ?? src.tabIds[i - 1] ?? null;
    const at = src === dst && i < toIndex ? toIndex - 1 : toIndex;
    dst.tabIds.splice(Math.max(0, Math.min(at, dst.tabIds.length)), 0, id);
    dst.activeId = id;
    this.focusedGroup = dst.id;
    this.#collapse(src.id);
  }

  /** Moves a tab to the end of another group (no-op if it is already there). */
  moveToGroup(id: string, group: Placement) {
    const from = this.groupOf(id);
    if (!from) return;
    // Splitting a group off its only tab would just move the tab into itself.
    const base = group === "beside" ? this.focusedGroup : typeof group === "object" ? (group.of ?? this.focusedGroup) : null;
    // ("beside" only creates a group when there is no other one.)
    const creates = typeof group === "object" || (group === "beside" && !this.isSplit);
    if (base === from && creates && !this.canSplit(id)) return;
    if (group === from) return;
    const to = this.#resolve(group);
    if (to === from) return;
    this.move(id, this.group(to)?.tabIds.length ?? 0, to);
  }

  /** Can this tab be split off into a group of its own (needs a tab left behind)? */
  canSplit(id: string): boolean {
    const g = this.groups.find((g) => g.tabIds.includes(id));
    return !!g && g.tabIds.length > 1;
  }

  /** Neighbouring groups of the group holding `id`, by direction. */
  neighbors(id: string): Partial<Record<Direction, string>> {
    const from = this.groupOf(id);
    const out: Partial<Record<Direction, string>> = {};
    if (!from) return out;
    for (const dir of ["left", "right", "up", "down"] as const) {
      const n = neighbor(this.layout, from, dir);
      if (n) out[dir] = n;
    }
    return out;
  }

  setRatio(splitId: string, ratio: number) {
    const split = findSplit(this.layout, splitId);
    if (split) split.ratio = clampRatio(ratio);
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
