import type { Host, HostDraft, HostId } from "../types";
import { collator } from "../util/format";
import { attempt, call } from "./backend";

class Hosts {
  list = $state<Host[]>([]);
  warnings = $state<string[]>([]);
  loaded = $state(false);
  reloading = $state(false);
  /** Old id -> current id of renamed hosts. */
  renamed = $state<Record<HostId, HostId>>({});
  /** Where new hosts are added (null until loaded). */
  newHostFile = $state<string | null>(null);

  byId = $derived(new Map(this.list.map((h) => [h.id, h])));
  /** Lower-cased alias -> ssh_config host, for resolving ProxyJump hops. */
  byName = $derived(
    new Map(this.list.filter((h) => h.source !== "tailscale").map((h) => [h.name.toLowerCase(), h])),
  );
  groups = $derived(
    [...new Set(this.list.map((h) => h.group).filter((g): g is string => !!g))].sort(collator.compare),
  );
  tags = $derived([...new Set(this.list.flatMap((h) => h.tags))].sort(collator.compare));

  get(id: HostId | null | undefined): Host | undefined {
    return id ? this.byId.get(id) : undefined;
  }

  findByName(name: string): Host | undefined {
    return this.byName.get(name.trim().toLowerCase());
  }

  /** The id host `id` has now (following renames), or null if it is gone. */
  currentId(id: HostId): HostId | null {
    const current = this.renamed[id] ?? id;
    return this.byId.has(current) ? current : null;
  }

  async refresh() {
    const [list, warnings, renamed, newHostFile] = await Promise.all([
      attempt((b) => b.listHosts(), { action: "Couldn't load hosts" }),
      attempt((b) => b.hostWarnings(), { toast: false }),
      attempt((b) => b.hostRenames(), { toast: false }),
      attempt((b) => b.newHostFile(), { toast: false }),
    ]);
    if (list) this.list = list;
    if (warnings) this.warnings = warnings;
    if (renamed) this.renamed = renamed;
    if (newHostFile) this.newHostFile = newHostFile;
    this.loaded = true;
  }

  /** Re-reads ~/.ssh/config from disk. */
  async reload() {
    this.reloading = true;
    try {
      const list = await attempt((b) => b.reloadHosts(), { action: "Couldn't reload ssh config" });
      if (list) this.list = list;
      const warnings = await attempt((b) => b.hostWarnings(), { toast: false });
      if (warnings) this.warnings = warnings;
    } finally {
      this.reloading = false;
    }
  }

  // CRUD rethrows (without toasting) so the editor can show errors inline.
  async create(draft: HostDraft): Promise<Host> {
    const host = await call((b) => b.createHost(draft), { toast: false });
    this.#upsert(host);
    return host;
  }

  /** Saves a host in its config file; renaming it changes its id (see `renamed`). */
  async update(id: HostId, draft: HostDraft): Promise<Host> {
    const host = await call((b) => b.updateHost(id, draft), { toast: false });
    if (host.id !== id) {
      this.renamed = { ...this.renamed, [id]: host.id };
      this.list = this.list.filter((h) => h.id !== id);
    }
    this.#upsert(host);
    return host;
  }

  /** Sets the group and tags of a config host (read-only ones too). */
  async organize(id: HostId, group: string | null, tags: string[]): Promise<Host> {
    const host = await call((b) => b.organizeHost(id, group, tags), { toast: false });
    this.#upsert(host);
    return host;
  }

  async remove(id: HostId): Promise<boolean> {
    try {
      await call((b) => b.deleteHost(id), { action: "Couldn't delete host" });
      this.list = this.list.filter((h) => h.id !== id);
      return true;
    } catch {
      return false;
    }
  }

  // Apply results immediately; the hostsChanged event refreshes afterwards anyway.
  #upsert(host: Host) {
    const i = this.list.findIndex((h) => h.id === host.id);
    this.list = i === -1 ? [...this.list, host] : this.list.map((h, j) => (j === i ? host : h));
  }
}

export const hosts = new Hosts();
