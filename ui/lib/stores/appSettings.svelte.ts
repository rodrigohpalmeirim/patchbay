// Settings the backend acts on (tray, autostart, transfer parallelism).
// UI-only preferences live in settings.svelte.ts (localStorage). Changes made
// elsewhere (e.g. the tray menu) arrive as `settingsChanged` events.

import type { AppSettings } from "../types";
import { attempt, call } from "./backend";

export const MAX_TRANSFERS_LIMITS = [1, 8] as const;

class AppSettingsStore {
  value = $state<AppSettings | null>(null);
  saving = $state(false);
  #loading: Promise<void> | null = null;
  /** Bumped per write so an older response can't overwrite a newer choice. */
  #seq = 0;

  load(): Promise<void> {
    this.#loading ??= attempt((b) => b.settingsGet(), { action: "Couldn't load settings" })
      .then((s) => {
        if (s) this.value = s;
      })
      .finally(() => (this.#loading = null));
    return this.#loading;
  }

  /** Applies a `settingsChanged` event. */
  handle(s: AppSettings) {
    // Ignore echoes of in-flight writes; their response settles the value.
    if (!this.saving) this.value = s;
  }

  /** Optimistically applies `patch`, then stores what the backend accepted. */
  async set(patch: Partial<AppSettings>) {
    const prev = this.value;
    if (!prev) return;
    const next: AppSettings = { ...prev, ...patch };
    if (!next.launchAtLogin) next.startMinimized = false;
    this.value = next;
    const seq = ++this.#seq;
    this.saving = true;
    try {
      const saved = await call((b) => b.settingsSet(next), { action: "Couldn't save settings" });
      if (seq === this.#seq) this.value = saved;
    } catch {
      if (seq === this.#seq) this.value = prev;
    } finally {
      if (seq === this.#seq) this.saving = false;
    }
  }
}

export const appSettings = new AppSettingsStore();
