// This window among Patchbay's windows (src-tauri/src/window.rs). Every
// window runs the whole UI with its own tabs; the backend tells each one
// whether it is the "leader" (the oldest open window, which runs
// once-per-app work such as update checks) and whether it is the most
// recently focused one (which shows notifications about app-wide events, so
// they don't pop up in every window).
//
// Tabs move between windows (tab menu, or dragged out of the window): the
// source sends the tab's descriptor through the backend, which keeps it
// until the target takes it (`tabs-incoming`), and closes it here. Terminal
// tabs keep their shells and screens: their panes are handed over first
// (stores/terminals.svelte.ts); if the move fails they stay here. Restored
// tabs that were never shown (dormant) have no shells, so just the
// descriptor moves. A tab dragged onto another window (stores/dnd.svelte.ts)
// carries the drop point, where the target places it. Each window shows its
// active tab in its title and describes itself (that title, tab count) so
// the others can list it as a destination the same way.

import { isTauri } from "../api";
import { leaves } from "../terminal/layout";
import type { DraggedTab, ScreenPoint, TabDragOver, WindowSummary, WindowsInfo } from "../types";
import { toCommandError } from "../util/errors";
import { attempt, call } from "./backend";
import { placeMovedTab } from "./dnd.svelte";
import { hosts } from "./hosts.svelte";
import { connectIdle, needsConnection, parseTab } from "./session.svelte";
import { tabs, type Tab } from "./tabs.svelte";
import { terminals, type PaneHandover } from "./terminals.svelte";
import { toasts } from "./toasts.svelte";

/** Debounce for telling the other windows about this one. */
const DESCRIBE_DELAY = 250;

/** A tab on its way to another window (`windowMoveTab`). */
interface MovedTab {
  tab: Tab;
  /** Restored and never shown: no shells, just the descriptor. */
  dormant: boolean;
  /** Its terminal panes, by pane id. */
  panes: Record<string, PaneHandover>;
  /** Dropped onto the target window at this point of its viewport. */
  drop: ScreenPoint | null;
}

/** A window's title: its active tab's, else just the app's. */
export const windowTitle = (tabTitle: string | null) => (tabTitle ? `${tabTitle} — Patchbay` : "Patchbay");

const isObj = (v: unknown): v is Record<string, unknown> => !!v && typeof v === "object" && !Array.isArray(v);

class Windows {
  label = $state("main");
  /** Until the backend says otherwise, assume a single window. */
  leader = $state(false);
  active = $state(true);
  count = $state(1);
  /** The open windows, most recently focused first. */
  list = $state<WindowSummary[]>([]);
  /** Where this window's tabs can move to. */
  others = $derived(this.list.filter((w) => w.label !== this.label));
  /** Several windows need the desktop app. */
  readonly canOpen = isTauri;

  /** Tabs being moved out (one move at a time per tab). */
  #moving = new Set<string>();
  /** The app has started: tabs moved here can be shown. */
  #ready = false;
  #describeTimer: ReturnType<typeof setTimeout> | undefined;
  #title = "Patchbay";
  /** The next position of a tab dragged out of this window to send (`dragTab`). */
  #dragNext: { tab: DraggedTab | null; at: ScreenPoint | null } | null = null;
  #dragSending: Promise<void> | null = null;
  /** The window under that tab, as of the last position sent. */
  #dragOver: TabDragOver | null = null;

  /** Fetches this window's roles and follows changes. Returns a cleanup function. */
  async start(): Promise<() => void> {
    const unlisten = await Promise.all([
      call((b) => b.onWindowsChanged((info) => this.#apply(info)), { toast: false }),
      call((b) => b.onTabsIncoming(() => void this.#receive()), { toast: false }),
    ]);
    const info = await attempt((b) => b.windowInfo(), { toast: false });
    if (info) {
      this.label = info.label;
      this.#apply(info);
    }
    return () => {
      unlisten.forEach((u) => u());
      clearTimeout(this.#describeTimer);
    };
  }

  /** Hosts and tabs are loaded: show the tabs moved here (a new window's first ones). */
  ready() {
    this.#ready = true;
    void this.#receive();
  }

  #apply(info: WindowsInfo) {
    this.leader = info.leader === this.label;
    this.active = info.active === null || info.active === this.label;
    this.count = info.count;
    this.list = info.windows;
  }

  /**
   * How window `w` is listed as a destination: its title without the app's
   * name (an empty window's is just that), numbered when another destination
   * has the same one.
   */
  name(w: WindowSummary): string {
    const title = w.title ?? windowTitle(null);
    const alike = this.others.some((o) => o.label !== w.label && (o.title ?? windowTitle(null)) === title);
    return alike ? `${title} (window ${w.number})` : title;
  }

  /** Opens another, empty window next to this one. */
  open() {
    void attempt((b) => b.windowOpen(), { action: "Couldn't open a new window" });
  }

  /** Shows this window's active tab (its title) in the window's title, and tells the other windows. */
  describe(title: string | null, tabCount: number) {
    const full = windowTitle(title);
    if (full !== this.#title) {
      this.#title = full;
      void attempt((b) => b.windowSetTitle(full), { toast: false });
    }
    if (!this.canOpen) return;
    clearTimeout(this.#describeTimer);
    this.#describeTimer = setTimeout(() => void attempt((b) => b.windowDescribe(title, tabCount), { toast: false }), DESCRIBE_DELAY);
  }

  /**
   * Moves a tab to window `to` (dropped at point `drop` of its viewport, if
   * dragged there), or to a new window (with its tab strip at screen point
   * `at`, else next to this one). See the header.
   */
  async moveTab(tabId: string, to: string | null, at: ScreenPoint | null = null, drop: ScreenPoint | null = null) {
    const tab = tabs.get(tabId);
    if (!tab || !this.canOpen || this.#moving.has(tabId)) return;
    this.#moving.add(tabId);
    try {
      await this.#move(tab, to, at, drop);
    } finally {
      this.#moving.delete(tabId);
    }
  }

  async #move(tab: Tab, to: string | null, at: ScreenPoint | null, drop: ScreenPoint | null) {
    const dormant = tabs.isDormant(tab.id);
    const paneIds = tab.kind === "terminal" && !dormant ? leaves(tab.layout).map((l) => l.id) : [];
    let panes: Record<string, PaneHandover>;
    try {
      panes = await terminals.handOver(paneIds);
    } catch (e) {
      toasts.error(toCommandError(e), "Couldn't move the tab");
      return;
    }
    // Closed meanwhile (and its shells with it).
    if (!tabs.get(tab.id)) return;
    const moved: MovedTab = { tab: $state.snapshot(tab) as Tab, dormant, panes, drop: to ? drop : null };
    const ids = Object.values(panes).flatMap((p) => (p.terminalId ? [p.terminalId] : []));
    const target = await attempt((b) => b.windowMoveTab(moved, ids, to, at), { action: "Couldn't move the tab" });
    if (target === undefined) {
      terminals.resume(paneIds);
      return;
    }
    terminals.release(paneIds);
    tabs.close(tab.id);
  }

  /**
   * A tab dragged out of this window is at screen point `at`: shown in the
   * window under it (stores/dnd.svelte.ts). Positions go one at a time; one
   * superseded meanwhile is skipped.
   */
  dragTab(tab: DraggedTab, at: ScreenPoint) {
    if (this.canOpen) this.#sendDrag({ tab, at });
  }

  /** That tab came back, or the drag ended: no other window shows it any more. */
  dragLeave() {
    if (this.#dragOver || this.#dragSending) this.#sendDrag({ tab: null, at: null });
  }

  /** That tab was released at screen point `at`: the window it was dropped on and where, if any. */
  async dragDrop(tab: DraggedTab, at: ScreenPoint): Promise<TabDragOver | null> {
    if (!this.canOpen) return null;
    this.#sendDrag({ tab, at });
    await this.#dragSending;
    const over = this.#dragOver;
    this.dragLeave();
    return over;
  }

  #sendDrag(next: { tab: DraggedTab | null; at: ScreenPoint | null }) {
    this.#dragNext = next;
    this.#dragSending ??= (async () => {
      while (this.#dragNext) {
        const { tab, at } = this.#dragNext;
        this.#dragNext = null;
        this.#dragOver = (await attempt((b) => b.windowDragTab(tab, at), { toast: false })) ?? null;
      }
      this.#dragSending = null;
    })();
  }

  /** Shows the tabs moved here. */
  async #receive() {
    if (!this.#ready) return;
    const moved = (await attempt((b) => b.windowTakeTabs(), { toast: false })) ?? [];
    for (const m of moved) this.#insert(m);
  }

  #insert(raw: unknown) {
    const m = isObj(raw) ? raw : {};
    const panes: Record<string, PaneHandover> = isObj(m.panes) ? (m.panes as Record<string, PaneHandover>) : {};
    const tab = parseTab(m.tab, (id) => hosts.currentId(id));
    const paneIds = new Set(tab?.kind === "terminal" ? leaves(tab.layout).map((l) => l.id) : []);
    // Shells that can't be shown here (e.g. their host was deleted meanwhile) are closed.
    for (const [paneId, p] of Object.entries(panes)) {
      const id = p.terminalId;
      if (paneIds.has(paneId) && tab && !tabs.get(tab.id)) continue;
      delete panes[paneId];
      if (id) void attempt((b) => b.terminalClose(id), { toast: false });
    }
    if (!tab || tabs.get(tab.id)) return;
    terminals.expect(panes);
    const dormant = m.dormant === true;
    const drop = isObj(m.drop) && typeof m.drop.x === "number" && typeof m.drop.y === "number" ? { x: m.drop.x, y: m.drop.y } : null;
    const shown = drop ? placeMovedTab(tab, drop.x, drop.y, dormant) : tabs.insert(tab, { dormant });
    if (shown.kind === "terminal") terminals.requestFocus(shown.id);
    if (needsConnection(shown) && !tabs.isDormant(shown.id)) connectIdle(shown.hostId);
  }
}

export const windows = new Windows();
