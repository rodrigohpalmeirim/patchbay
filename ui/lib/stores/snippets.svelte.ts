// Saved commands. The backend persists them; this mirrors the list and
// reloads it on `snippetsChanged`.

import type { HostId, Snippet, SnippetDraft } from "../types";
import { collator } from "../util/format";
import { attempt, call } from "./backend";

class Snippets {
  list = $state<Snippet[]>([]);
  loaded = $state(false);
  #loading: Promise<void> | null = null;

  sorted = $derived([...this.list].sort((a, b) => collator.compare(a.name, b.name)));

  load(): Promise<void> {
    this.#loading ??= attempt((b) => b.snippetsList(), { action: "Couldn't load snippets" })
      .then((list) => {
        if (list) this.list = list;
        this.loaded = true;
      })
      .finally(() => (this.#loading = null));
    return this.#loading;
  }

  /** Global snippets plus those scoped to `hostId` (local terminals: global only). */
  forHost(hostId: HostId | null): Snippet[] {
    return this.sorted.filter((s) => s.hostIds.length === 0 || (hostId !== null && s.hostIds.includes(hostId)));
  }

  async create(draft: SnippetDraft): Promise<Snippet> {
    const s = await call((b) => b.snippetCreate(draft), { action: "Couldn't save snippet" });
    this.#upsert(s);
    return s;
  }

  async update(id: string, draft: SnippetDraft): Promise<Snippet> {
    const s = await call((b) => b.snippetUpdate(id, draft), { action: "Couldn't save snippet" });
    this.#upsert(s);
    return s;
  }

  async remove(id: string): Promise<void> {
    await call((b) => b.snippetDelete(id), { action: "Couldn't delete snippet" });
    this.list = this.list.filter((s) => s.id !== id);
  }

  #upsert(s: Snippet) {
    const i = this.list.findIndex((x) => x.id === s.id);
    this.list = i === -1 ? [...this.list, s] : this.list.map((x) => (x.id === s.id ? s : x));
  }
}

export const snippets = new Snippets();
