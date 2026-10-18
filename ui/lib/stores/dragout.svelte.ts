// Dragging file rows out of the window to the OS (desktop, Finder, Explorer).
//
// A native OS drag needs real local paths the moment it starts. Local
// entries have them; remote ones are copied into a temporary folder by a
// background transfer as soon as an internal drag of them begins
// (`dragoutPrepare`), so the copy is usually ready by the time the pointer
// leaves the window. The dnd store hands the paths to `startNativeDrag`
// (tauri-plugin-drag) once the pointer is outside the window with the button
// held and the copy is ready; until then the drag ghost shows the progress.
// Copies larger than DRAGOUT_MAX_BYTES are refused (use "Download to…").
// The backend deletes copies some time after the drop and on exit; copies
// of drags that end inside the window are deleted right away (`release`).

import type { FileEntry, HostId } from "../types";
import { toCommandError } from "../util/errors";
import { attempt, call } from "./backend";
import { transfers } from "./transfers.svelte";

/** Mirrors `patchbay_core::dragout::MAX_BYTES`. */
export const DRAGOUT_MAX_BYTES = 200 * 1024 * 1024;

export type DragOutStatus = "preparing" | "ready" | "tooLarge" | "failed";

/** Local paths for one drag, ready at once (local entries) or being copied. */
export class DragOut {
  status = $state<DragOutStatus>("preparing");
  paths = $state<string[] | null>(null);
  error = $state<string | null>(null);
  transferId = $state<string | null>(null);
  /** 0..1 while copying; null when unknown. */
  progress = $derived.by(() => {
    const t = this.transferId ? transfers.map[this.transferId] : undefined;
    if (!t || t.totalBytes == null) return null;
    if (t.state === "done") return 1;
    return t.totalBytes > 0 ? Math.min(1, t.doneBytes / t.totalBytes) : null;
  });

  #copyId: string | null = null;
  #released = false;
  #stopWatching: (() => void) | null = null;

  /** `onReady` runs when the paths become available. */
  constructor(hostId: HostId | null, entries: FileEntry[], onReady: () => void) {
    if (hostId === null) {
      this.paths = entries.map((e) => e.path);
      this.status = "ready";
      return;
    }
    const known = entries.reduce((n, e) => n + (e.kind === "file" ? e.size : 0), 0);
    if (known > DRAGOUT_MAX_BYTES) {
      this.status = "tooLarge";
      return;
    }
    this.#stopWatching = $effect.root(() => {
      $effect(() => {
        const t = this.transferId ? transfers.map[this.transferId] : undefined;
        if (!t || this.status !== "preparing") return;
        if (t.state === "done") {
          this.status = "ready";
          queueMicrotask(onReady);
        } else if (t.state === "failed") {
          const tooLarge = (t.totalBytes ?? 0) > DRAGOUT_MAX_BYTES;
          this.status = tooLarge ? "tooLarge" : "failed";
          if (!tooLarge) this.error = t.message;
        } else if (t.state === "cancelled") {
          this.status = "failed";
        }
      });
    });
    void this.#prepare(hostId, entries);
  }

  async #prepare(hostId: HostId, entries: FileEntry[]) {
    try {
      const out = await call((b) => b.dragoutPrepare(entries.map((e) => ({ hostId, path: e.path }))), { toast: false });
      this.#copyId = out.id;
      if (this.#released) {
        this.release();
        return;
      }
      transfers.handle(out.transfer);
      this.paths = out.paths;
      this.transferId = out.transfer.id;
    } catch (e) {
      this.status = "failed";
      this.error = toCommandError(e).message;
    }
  }

  /** The drag ended inside the window: the copy isn't needed. */
  release() {
    this.#released = true;
    this.#stopWatching?.();
    this.#stopWatching = null;
    const id = this.#copyId;
    this.#copyId = null;
    if (id) void attempt((b) => b.dragoutRelease(id), { toast: false });
    if (this.transferId) transfers.forget(this.transferId);
  }

  /** Handed to the OS: keep the copy (the backend deletes it later). */
  detach() {
    this.#stopWatching?.();
    this.#stopWatching = null;
  }
}

/**
 * A small PNG (data URL) shown under the pointer during the native drag:
 * a file glyph plus the name or item count.
 */
export function dragIcon(entries: FileEntry[]): string {
  const label = entries.length === 1 ? entries[0].name : `${entries.length} items`;
  const canvas = document.createElement("canvas");
  const ctx = canvas.getContext("2d");
  if (!ctx) return "";
  const font = "500 13px system-ui, -apple-system, 'Segoe UI', sans-serif";
  ctx.font = font;
  const text = label.length > 40 ? `${label.slice(0, 38)}…` : label;
  const w = Math.ceil(Math.min(320, ctx.measureText(text).width + 44));
  const h = 30;
  canvas.width = w;
  canvas.height = h;
  ctx.font = font;
  ctx.fillStyle = "rgba(255,255,255,0.96)";
  ctx.strokeStyle = "rgba(0,0,0,0.18)";
  ctx.beginPath();
  ctx.roundRect(0.5, 0.5, w - 1, h - 1, 7);
  ctx.fill();
  ctx.stroke();
  if (entries.length > 1) {
    ctx.fillStyle = "#93c5fd";
    ctx.fillRect(14, 10, 12, 16);
  }
  // File glyph with a folded corner.
  ctx.fillStyle = "#3b82f6";
  ctx.beginPath();
  ctx.moveTo(11, 7);
  ctx.lineTo(19, 7);
  ctx.lineTo(23, 11);
  ctx.lineTo(23, 23);
  ctx.lineTo(11, 23);
  ctx.closePath();
  ctx.fill();
  ctx.fillStyle = "#111827";
  ctx.textBaseline = "middle";
  ctx.fillText(text, 32, h / 2 + 1, w - 40);
  return canvas.toDataURL("image/png");
}
