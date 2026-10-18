import type { CommandError } from "../types";
import { errorTitle, sentence } from "../util/errors";

export type ToastKind = "error" | "success" | "info";

export interface ToastAction {
  label: string;
  run: () => void;
}

export interface ToastOptions {
  action?: ToastAction;
  /** Stay until dismissed instead of timing out. */
  sticky?: boolean;
}

export interface Toast extends ToastOptions {
  id: number;
  kind: ToastKind;
  title: string;
  message?: string;
}

const TIMEOUT: Record<ToastKind, number> = { error: 7000, success: 3500, info: 4500 };

class Toasts {
  items = $state<Toast[]>([]);
  #next = 1;
  #timers = new Map<number, ReturnType<typeof setTimeout>>();

  show(kind: ToastKind, title: string, message?: string, options: ToastOptions = {}): number {
    const id = this.#next++;
    this.items = [...this.items.slice(-3), { id, kind, title, message, ...options }];
    if (!options.sticky) this.#arm(id, kind);
    return id;
  }

  /** Toasts a CommandError. Cancellations are intentionally silent. */
  error(error: CommandError, action?: string) {
    if (error.kind === "cancelled") return;
    this.show("error", action ?? errorTitle(error.kind), sentence(error.message));
  }

  success(title: string, message?: string) {
    this.show("success", title, message);
  }

  info(title: string, message?: string) {
    this.show("info", title, message);
  }

  dismiss(id: number) {
    clearTimeout(this.#timers.get(id));
    this.#timers.delete(id);
    this.items = this.items.filter((t) => t.id !== id);
  }

  /** Pause auto-dismiss while hovered/focused. */
  hold(id: number) {
    clearTimeout(this.#timers.get(id));
    this.#timers.delete(id);
  }

  release(id: number) {
    const t = this.items.find((x) => x.id === id);
    if (t && !t.sticky) this.#arm(id, t.kind);
  }

  #arm(id: number, kind: ToastKind) {
    clearTimeout(this.#timers.get(id));
    this.#timers.set(
      id,
      setTimeout(() => this.dismiss(id), TIMEOUT[kind]),
    );
  }
}

export const toasts = new Toasts();
