// Transfer queue mirror: seeded from transferList() on startup, then kept up
// to date by `transfer` core events. Also owns the bottom panel's UI state
// (transfers / editing tabs).
//
// Background transfers (drag-out copies) stay out of sight unless they are
// still running after REVEAL_AFTER_MS.

import { SvelteSet } from "svelte/reactivity";
import type { TransferInfo, TransferRequest } from "../types";
import { attempt, call } from "./backend";
import { filePrefs, files } from "./files.svelte";

export type TransferStateName = TransferInfo["state"];

const ACTIVE: ReadonlySet<TransferStateName> = new Set(["queued", "scanning", "running", "paused", "waitingForUser"]);
const FINISHED: ReadonlySet<TransferStateName> = new Set(["done", "failed", "cancelled"]);

export const isActive = (t: TransferInfo) => ACTIVE.has(t.state);
export const isFinished = (t: TransferInfo) => FINISHED.has(t.state);

export const PANEL_MIN = 120;
export const PANEL_MAX_FRACTION = 0.7;
const REVEAL_AFTER_MS = 1000;

export type PanelTab = "transfers" | "editing";

class Transfers {
  map = $state<Record<string, TransferInfo>>({});
  /** Background transfers that took long enough to be shown. */
  #revealed = new SvelteSet<string>();
  #revealTimers = new Map<string, ReturnType<typeof setTimeout>>();
  /** The user closed the panel; don't pop it open again this session. */
  #dismissed = false;
  tab = $state<PanelTab>("transfers");

  list = $derived(
    Object.values(this.map)
      .filter((t) => !t.background || this.#revealed.has(t.id))
      .sort((a, b) => b.createdAt - a.createdAt),
  );
  active = $derived(this.list.filter(isActive));
  failed = $derived(this.list.filter((t) => t.state === "failed"));
  finishedCount = $derived(this.list.filter(isFinished).length);

  /** Overall progress of active transfers (0..1), null when unknown. */
  progress = $derived.by(() => {
    const act = this.active.filter((t) => t.totalBytes != null);
    const total = act.reduce((n, t) => n + (t.totalBytes ?? 0), 0);
    if (!act.length || total <= 0) return null;
    return Math.min(1, act.reduce((n, t) => n + t.doneBytes, 0) / total);
  });

  get open(): boolean {
    return filePrefs.transfersOpen;
  }

  setOpen(open: boolean) {
    filePrefs.transfersOpen = open;
    if (!open) this.#dismissed = true;
    filePrefs.save();
  }

  toggle() {
    this.showTab("transfers");
  }

  /** Opens the panel on `tab`, or closes it if it already shows that tab. */
  showTab(tab: PanelTab) {
    const close = filePrefs.transfersOpen && this.tab === tab;
    this.tab = tab;
    this.setOpen(!close);
  }

  async load() {
    const list = await attempt((b) => b.transferList(), { toast: false });
    if (!list) return;
    const map: Record<string, TransferInfo> = {};
    for (const t of list) map[t.id] = t;
    this.map = map;
  }

  /** Applies a `transfer` event (or a command result). */
  handle(info: TransferInfo) {
    const prev = this.map[info.id];
    this.map[info.id] = info;
    if (info.background && !this.#revealTimers.has(info.id)) {
      this.#revealTimers.set(
        info.id,
        setTimeout(() => {
          const t = this.map[info.id];
          if (t && isActive(t)) this.#revealed.add(info.id);
        }, REVEAL_AFTER_MS),
      );
    }
    if (prev && prev.state !== info.state && isFinished(info)) {
      // Show new/partial files wherever the destination is open.
      files.refreshDir(info.destination.hostId, info.destination.path);
    }
  }

  /** Drops a transfer from view (a background copy that is no longer needed). */
  forget(id: string) {
    delete this.map[id];
    this.#revealed.delete(id);
  }

  async start(request: TransferRequest): Promise<TransferInfo | undefined> {
    const info = await attempt((b) => b.transferStart(request), { action: "Couldn't start transfer" });
    if (!info) return undefined;
    if (!this.map[info.id]) this.map[info.id] = info;
    if (!filePrefs.transfersOpen && !this.#dismissed) {
      this.tab = "transfers";
      filePrefs.transfersOpen = true;
      filePrefs.save();
    }
    return info;
  }

  pause(id: string) {
    return attempt((b) => b.transferPause(id), { action: "Couldn't pause transfer" });
  }

  resume(id: string) {
    return attempt((b) => b.transferResume(id), { action: "Couldn't resume transfer" });
  }

  cancel(id: string) {
    return attempt((b) => b.transferCancel(id), { action: "Couldn't cancel transfer" });
  }

  retry(id: string) {
    return attempt((b) => b.transferRetry(id), { action: "Couldn't retry transfer" });
  }

  async clearFinished() {
    await call((b) => b.transferClearFinished(), { action: "Couldn't clear transfers" }).catch(() => undefined);
    const map: Record<string, TransferInfo> = {};
    for (const t of Object.values(this.map)) if (!isFinished(t)) map[t.id] = t;
    this.map = map;
    for (const id of this.#revealed) if (!map[id]) this.#revealed.delete(id);
  }
}

export const transfers = new Transfers();
