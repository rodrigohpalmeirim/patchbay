// File browser state. One `FileBrowser` per files tab (keyed by tab id) holds
// the location, history, listing, selection, filter and search; views render
// it and actions (files/ops.ts) mutate it. Keeping this out of components lets
// drag-and-drop, transfers and the split view talk to any pane.

import type { CommandError, CoreEvent, DirListing, FileEntry, HostId } from "../types";
import { toCommandError } from "../util/errors";
import { isDirLike } from "../util/fileIcons";
import { collator } from "../util/format";
import { basename, dirname, normalize } from "../util/paths";
import { attempt, call } from "./backend";
import { tabs, type FilesTab } from "./tabs.svelte";

export type SortKey = "name" | "size" | "modified" | "permissions" | "owner";
export type SortDir = "asc" | "desc";

// ------------------------------------------------------------------ prefs
//
// Persisted in localStorage and handed to the other windows as they change;
// whether the preview and transfer panels are open stays per window.

const PREFS_KEY = "patchbay.files.v1";
/** Topics for sharing with the other windows. */
export const PREFS_TOPIC = "filePrefs";
export const CLIPBOARD_TOPIC = "fileClipboard";

interface Prefs {
  showHidden: boolean;
  sortKey: SortKey;
  sortDir: SortDir;
  previewOpen: boolean;
  previewWidth: number;
  transfersOpen: boolean;
  transfersHeight: number;
}

const DEFAULT_PREFS: Prefs = {
  showHidden: false,
  sortKey: "name",
  sortDir: "asc",
  previewOpen: false,
  previewWidth: 320,
  transfersOpen: false,
  transfersHeight: 240,
};

function loadPrefs(): Prefs {
  try {
    const raw = localStorage.getItem(PREFS_KEY);
    return raw ? { ...DEFAULT_PREFS, ...(JSON.parse(raw) as Partial<Prefs>) } : { ...DEFAULT_PREFS };
  } catch {
    return { ...DEFAULT_PREFS };
  }
}

const SORT_KEYS: readonly SortKey[] = ["name", "size", "modified", "permissions", "owner"];
const num = (v: unknown, fallback: number) => (typeof v === "number" && Number.isFinite(v) ? v : fallback);

class FilePrefs {
  #p = loadPrefs();
  showHidden = $state(this.#p.showHidden);
  sortKey = $state<SortKey>(this.#p.sortKey);
  sortDir = $state<SortDir>(this.#p.sortDir);
  previewOpen = $state(this.#p.previewOpen);
  previewWidth = $state(this.#p.previewWidth);
  transfersOpen = $state(this.#p.transfersOpen);
  transfersHeight = $state(this.#p.transfersHeight);

  save() {
    const data: Prefs = {
      showHidden: this.showHidden,
      sortKey: this.sortKey,
      sortDir: this.sortDir,
      previewOpen: this.previewOpen,
      previewWidth: this.previewWidth,
      transfersOpen: this.transfersOpen,
      transfersHeight: this.transfersHeight,
    };
    try {
      localStorage.setItem(PREFS_KEY, JSON.stringify(data));
    } catch {
      // Not persisted; fine.
    }
    void attempt((b) => b.share(PREFS_TOPIC, data), { toast: false });
  }

  /** Takes over prefs changed (and already saved) in another window. */
  adopt(raw: unknown) {
    const p = (raw && typeof raw === "object" ? raw : {}) as Partial<Record<keyof Prefs, unknown>>;
    if (typeof p.showHidden === "boolean") this.showHidden = p.showHidden;
    if (SORT_KEYS.includes(p.sortKey as SortKey)) this.sortKey = p.sortKey as SortKey;
    if (p.sortDir === "asc" || p.sortDir === "desc") this.sortDir = p.sortDir;
    this.previewWidth = num(p.previewWidth, this.previewWidth);
    this.transfersHeight = num(p.transfersHeight, this.transfersHeight);
  }
}

export const filePrefs = new FilePrefs();

// ----------------------------------------------------------------- search

export interface SearchState {
  query: string;
  root: string;
  id: string | null;
  results: FileEntry[];
  running: boolean;
  error: string | null;
}

// ---------------------------------------------------------------- browser

/** Things a row needs to compare on, precomputed once per listing. */
function compare(key: SortKey, a: FileEntry, b: FileEntry): number {
  switch (key) {
    case "size":
      return a.size - b.size;
    case "modified":
      return (a.modified ?? 0) - (b.modified ?? 0);
    case "permissions":
      return (a.permissions ?? 0) - (b.permissions ?? 0);
    case "owner":
      return collator.compare(a.owner ?? "", b.owner ?? "");
    default:
      return 0;
  }
}

export class FileBrowser {
  readonly tabId: string;
  readonly hostId: HostId | null;

  path = $state<string | null>(null);
  home = $state<string | null>(null);
  listing = $state.raw<DirListing | null>(null);
  loading = $state(false);
  error = $state<CommandError | null>(null);
  /** Listing shown is from before a disconnect. */
  stale = $state(false);

  history = $state<string[]>([]);
  historyIndex = $state(-1);

  selected = $state.raw<ReadonlySet<string>>(new Set());
  /** Focused row (keyboard cursor). */
  cursor = $state<string | null>(null);
  /** Anchor for shift-range selection. */
  anchor: string | null = null;

  filter = $state("");
  filterOpen = $state(false);
  renaming = $state<string | null>(null);
  creating = $state<"dir" | "file" | null>(null);
  search = $state<SearchState | null>(null);
  searchOpen = $state(false);
  /** Preview panel shown in this pane (new panes start with the last choice). */
  previewOpen = $state(filePrefs.previewOpen);
  /** Bumped to ask the list to scroll the cursor into view. */
  revealTick = $state(0);
  /** Bumped to ask the view to focus the list. */
  focusTick = $state(0);

  #loadSeq = 0;

  constructor(tab: FilesTab) {
    this.tabId = tab.id;
    this.hostId = tab.hostId;
    this.path = tab.path;
  }

  canBack = $derived(this.historyIndex > 0);
  canForward = $derived(this.historyIndex < this.history.length - 1);
  parent = $derived(this.listing?.parent ?? (this.path ? dirname(this.path) : null));

  /** Entries in display order (filtered, sorted, dirs first). */
  rows = $derived.by((): FileEntry[] => {
    const src = this.search ? this.search.results : (this.listing?.entries ?? []);
    const q = this.filter.trim().toLowerCase();
    const showHidden = filePrefs.showHidden;
    const out = src.filter((e) => (showHidden || !e.hidden) && (!q || e.name.toLowerCase().includes(q)));
    const key = filePrefs.sortKey;
    const dir = filePrefs.sortDir === "asc" ? 1 : -1;
    out.sort((a, b) => {
      const da = isDirLike(a) ? 0 : 1;
      const db = isDirLike(b) ? 0 : 1;
      if (da !== db) return da - db;
      const c = key === "name" ? 0 : compare(key, a, b);
      return (c || collator.compare(a.name, b.name) || collator.compare(a.path, b.path)) * dir;
    });
    return out;
  });

  /** path -> row index, for O(1) selection math. */
  rowIndex = $derived(new Map(this.rows.map((e, i) => [e.path, i])));

  hiddenCount = $derived(
    filePrefs.showHidden ? 0 : (this.search ? this.search.results : (this.listing?.entries ?? [])).filter((e) => e.hidden).length,
  );

  selectedEntries = $derived(this.rows.filter((e) => this.selected.has(e.path)));

  get title(): string {
    return this.path ? basename(this.path) : "Files";
  }

  // ------------------------------------------------------------ loading

  /** Resolves the start directory (home if the tab has none) and loads it. */
  async init(): Promise<void> {
    if (this.listing && !this.stale) return;
    if (this.stale && this.path) return this.refresh();
    if (!this.home) {
      this.home = await attempt((b) => b.fsHome(this.hostId), { toast: false }).then((h) => h ?? null);
    }
    const start = this.path ?? this.home;
    if (start != null) await this.navigate(start);
    else this.error = { kind: "other", message: "couldn't determine the home directory" };
  }

  async navigate(path: string, { push = true, select }: { push?: boolean; select?: string } = {}) {
    const target = normalize(path);
    const seq = ++this.#loadSeq;
    this.loading = true;
    this.cancelSearch();
    try {
      const listing = await call((b) => b.fsList(this.hostId, target), { toast: false });
      if (seq !== this.#loadSeq) return;
      const changed = listing.path !== this.path;
      this.listing = listing;
      this.error = null;
      this.stale = false;
      this.#setPath(listing.path, push && changed);
      if (changed) {
        this.creating = null;
        this.renaming = null;
        this.filter = "";
        this.filterOpen = false;
        this.selected = new Set();
        this.cursor = null;
        this.anchor = null;
      }
      if (select && listing.entries.some((e) => e.path === select)) {
        this.selectOnly(select);
        this.revealTick++;
      }
    } catch (e) {
      if (seq !== this.#loadSeq) return;
      this.error = toCommandError(e);
      if (this.error.kind === "notConnected") {
        this.stale = !!this.listing;
        return;
      }
      this.listing = null;
      this.selected = new Set();
      this.cursor = null;
      this.#setPath(target, push && target !== this.path);
    } finally {
      if (seq === this.#loadSeq) this.loading = false;
    }
  }

  #setPath(path: string, push: boolean) {
    this.path = path;
    if (push) {
      this.history = [...this.history.slice(0, this.historyIndex + 1), path];
      this.historyIndex = this.history.length - 1;
    } else if (this.historyIndex === -1) {
      this.history = [path];
      this.historyIndex = 0;
    }
    tabs.update<FilesTab>(this.tabId, { path });
  }

  /** Reloads the current directory, keeping selection where possible. */
  async refresh(): Promise<void> {
    if (this.path == null) return this.init();
    const seq = ++this.#loadSeq;
    this.loading = true;
    try {
      const listing = await call((b) => b.fsList(this.hostId, this.path!), { toast: false });
      if (seq !== this.#loadSeq) return;
      this.listing = listing;
      this.error = null;
      this.stale = false;
      const paths = new Set(listing.entries.map((e) => e.path));
      this.selected = new Set([...this.selected].filter((p) => paths.has(p)));
      if (this.cursor && !paths.has(this.cursor)) this.cursor = null;
    } catch (e) {
      if (seq !== this.#loadSeq) return;
      const err = toCommandError(e);
      if (err.kind === "notConnected") {
        this.stale = !!this.listing;
        this.error = this.listing ? null : err;
      } else {
        this.error = err;
        this.listing = null;
      }
    } finally {
      if (seq === this.#loadSeq) this.loading = false;
    }
  }

  back() {
    if (!this.canBack) return;
    this.historyIndex--;
    void this.navigate(this.history[this.historyIndex], { push: false });
  }

  forward() {
    if (!this.canForward) return;
    this.historyIndex++;
    void this.navigate(this.history[this.historyIndex], { push: false });
  }

  up() {
    const from = this.path;
    if (this.parent != null && from) void this.navigate(this.parent, { select: from });
  }

  /** Marks the listing as from a lost connection. */
  markStale() {
    if (this.listing) this.stale = true;
    this.cancelSearch();
  }

  // ---------------------------------------------------------- selection

  selectOnly(path: string | null) {
    this.selected = new Set(path ? [path] : []);
    this.cursor = path;
    this.anchor = path;
  }

  toggle(path: string) {
    const next = new Set(this.selected);
    if (next.has(path)) next.delete(path);
    else next.add(path);
    this.selected = next;
    this.cursor = path;
    this.anchor = path;
  }

  /** Selects from the anchor to `path` (inclusive). */
  selectRange(path: string, additive = false) {
    const a = this.rowIndex.get(this.anchor ?? path) ?? 0;
    const b = this.rowIndex.get(path) ?? 0;
    const [lo, hi] = a < b ? [a, b] : [b, a];
    const next = additive ? new Set(this.selected) : new Set<string>();
    for (let i = lo; i <= hi; i++) next.add(this.rows[i].path);
    this.selected = next;
    this.cursor = path;
  }

  selectAll() {
    this.selected = new Set(this.rows.map((e) => e.path));
  }

  clearSelection() {
    this.selected = new Set();
  }

  // ------------------------------------------------------------- search

  openSearch() {
    this.searchOpen = true;
  }

  #searchSeq = 0;

  async startSearch(query: string) {
    const root = this.path;
    if (!root || !query.trim()) return;
    this.cancelSearch();
    this.searchOpen = true;
    const seq = ++this.#searchSeq;
    const q = query.trim();
    this.search = { query: q, root, id: null, results: [], running: true, error: null };
    this.selected = new Set();
    this.cursor = null;
    try {
      const id = await call((b) => b.searchStart(this.hostId, root, q), { toast: false });
      if (seq !== this.#searchSeq || !this.search) {
        void attempt((b) => b.searchCancel(id), { toast: false });
        return;
      }
      this.search.id = id;
      searches.register(id, this);
    } catch (e) {
      if (seq === this.#searchSeq && this.search) {
        this.search.running = false;
        this.search.error = toCommandError(e).message;
      }
    }
  }

  /** Applies a streamed `search` event. */
  onSearchEvent(ev: Extract<CoreEvent, { type: "search" }>) {
    const s = this.search;
    if (!s || s.id !== ev.searchId) return;
    if (ev.entries.length) s.results = [...s.results, ...ev.entries];
    if (ev.error) s.error = ev.error;
    if (ev.done) {
      s.running = false;
      searches.unregister(ev.searchId);
    }
  }

  stopSearch() {
    const s = this.search;
    if (!s?.id || !s.running) return;
    void attempt((b) => b.searchCancel(s.id!), { toast: false });
    searches.unregister(s.id);
    s.running = false;
  }

  /** Stops any search and leaves search mode. */
  cancelSearch() {
    this.#searchSeq++;
    this.stopSearch();
    if (this.search) {
      this.search = null;
      this.selected = new Set();
      this.cursor = null;
    }
    this.searchOpen = false;
  }

  dispose() {
    this.stopSearch();
    this.#loadSeq++;
  }
}

// ---------------------------------------------------------------- routing

/** Routes `search` events to the browser that started them. */
class Searches {
  #owners = new Map<string, FileBrowser>();
  /** Events that arrived before searchStart resolved with their id. */
  #early = new Map<string, Extract<CoreEvent, { type: "search" }>[]>();

  register(id: string, owner: FileBrowser) {
    this.#owners.set(id, owner);
    const early = this.#early.get(id);
    this.#early.delete(id);
    early?.forEach((ev) => owner.onSearchEvent(ev));
  }

  unregister(id: string) {
    this.#owners.delete(id);
    this.#early.delete(id);
  }

  handle(ev: Extract<CoreEvent, { type: "search" }>) {
    const owner = this.#owners.get(ev.searchId);
    if (owner) return owner.onSearchEvent(ev);
    if (this.#early.size > 32) this.#early.clear();
    this.#early.set(ev.searchId, [...(this.#early.get(ev.searchId) ?? []), ev]);
  }
}

export const searches = new Searches();

export interface Clip {
  hostId: HostId | null;
  entries: FileEntry[];
}

class Files {
  #browsers = new Map<string, FileBrowser>();
  /** Copied entries (Ctrl/Cmd+C) waiting to be pasted into a pane, in any window. */
  clipboard = $state.raw<Clip | null>(null);

  setClipboard(clip: Clip | null) {
    this.clipboard = clip;
    void attempt((b) => b.share(CLIPBOARD_TOPIC, clip), { toast: false });
  }

  /** Takes over what was copied in another window. */
  adoptClipboard(raw: unknown) {
    const c = raw as Partial<Clip> | null;
    const valid = !!c && (c.hostId === null || typeof c.hostId === "string") && Array.isArray(c.entries);
    this.clipboard = valid ? { hostId: c.hostId ?? null, entries: c.entries ?? [] } : null;
  }

  ensure(tab: FilesTab): FileBrowser {
    let b = this.#browsers.get(tab.id);
    if (!b) {
      b = new FileBrowser(tab);
      this.#browsers.set(tab.id, b);
    }
    return b;
  }

  get(tabId: string): FileBrowser | undefined {
    return this.#browsers.get(tabId);
  }

  dispose(tabId: string) {
    this.#browsers.get(tabId)?.dispose();
    this.#browsers.delete(tabId);
  }

  all(): FileBrowser[] {
    return [...this.#browsers.values()];
  }

  /** Refreshes every pane showing `dir` on that machine. */
  refreshDir(hostId: HostId | null, dir: string) {
    const d = normalize(dir);
    for (const b of this.#browsers.values()) {
      if (b.hostId === hostId && b.path && normalize(b.path) === d && !b.search) void b.refresh();
    }
  }

  /** Files panes showing in the other tab groups (left to right, top to bottom). */
  otherPanes(tabId: string): FileBrowser[] {
    const own = tabs.groupOf(tabId);
    return tabs.order
      .filter((id) => id !== own)
      .map((id) => this.#browsers.get(tabs.group(id)?.activeId ?? ""))
      .filter((b): b is FileBrowser => !!b && b.tabId !== tabId);
  }

  /** Lost connection: keep listings, mark stale. */
  onDisconnected(hostId: HostId) {
    for (const b of this.#browsers.values()) if (b.hostId === hostId) b.markStale();
  }
}

export const files = new Files();
