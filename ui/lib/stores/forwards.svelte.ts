// Port forward rules and their live status. The backend owns both; this
// mirrors the rule list (reloaded on `portForwardsChanged`) and applies
// `portForward` status events, deriving transfer rates from the counters.

import type { HostId, PortForward, PortForwardDraft, PortForwardStatus } from "../types";
import { toCommandError } from "../util/errors";
import { collator } from "../util/format";
import { attempt, call } from "./backend";
import { hosts } from "./hosts.svelte";
import { toasts } from "./toasts.svelte";

export interface Rate {
  /** Bytes per second, smoothed. */
  in: number;
  out: number;
}

interface Sample {
  t: number;
  bytesIn: number;
  bytesOut: number;
}

/** Forward states that hold a listener (or are trying to). */
export function isRunning(s: PortForwardStatus | undefined): boolean {
  return !!s && (s.state === "active" || s.state === "starting" || s.state === "reconnecting");
}

const idle = (id: string): PortForwardStatus => ({
  id,
  state: "stopped",
  openConnections: 0,
  totalConnections: 0,
  bytesIn: 0,
  bytesOut: 0,
});

/** Rates decay to zero when no counter update arrives for this long. */
const STALE_MS = 2500;
/** Smoothing factor for the rate EMA (weight of the newest sample). */
const ALPHA = 0.5;

class Forwards {
  list = $state<PortForward[]>([]);
  statuses = $state<Record<string, PortForwardStatus>>({});
  rates = $state<Record<string, Rate>>({});
  loaded = $state(false);
  /** Rules with a start/stop request in flight (the toggle shows it). */
  pending = $state<Record<string, boolean>>({});

  #samples = new Map<string, Sample>();
  #ticker: ReturnType<typeof setInterval> | undefined;
  #loading: Promise<void> | null = null;
  #again = false;

  byId = $derived(new Map(this.list.map((f) => [f.id, f])));
  sorted = $derived([...this.list].sort((a, b) => collator.compare(a.name, b.name)));
  activeCount = $derived(this.list.filter((f) => this.statuses[f.id]?.state === "active").length);
  /** Rules that need attention (failed or reconnecting). */
  troubleCount = $derived(
    this.list.filter((f) => {
      const s = this.statuses[f.id]?.state;
      return s === "failed" || s === "reconnecting";
    }).length,
  );

  get(id: string): PortForward | undefined {
    return this.byId.get(id);
  }

  status(id: string): PortForwardStatus {
    return this.statuses[id] ?? idle(id);
  }

  rate(id: string): Rate {
    return this.rates[id] ?? { in: 0, out: 0 };
  }

  /** Rules carried by `hostId` (it listens / connects out). */
  onHost(hostId: HostId): PortForward[] {
    return this.sorted.filter((f) => f.hostId === hostId);
  }

  /** Remote rules whose traffic leaves from `hostId` (machine-to-machine). */
  viaHost(hostId: HostId): PortForward[] {
    return this.sorted.filter((f) => f.viaHostId === hostId && f.hostId !== hostId);
  }

  /** Running rules that involve `hostId` either way. */
  activeFor(hostId: HostId): PortForward[] {
    return this.sorted.filter((f) => (f.hostId === hostId || f.viaHostId === hostId) && this.statuses[f.id]?.state === "active");
  }

  /** Reloads rules and statuses. Calls during a load queue one more load. */
  load(): Promise<void> {
    if (this.#loading) {
      this.#again = true;
      return this.#loading;
    }
    this.#loading = (async () => {
      do {
        this.#again = false;
        const [list, statuses] = await Promise.all([
          attempt((b) => b.forwardsList(), { action: "Couldn't load port forwards" }),
          attempt((b) => b.forwardStatuses(), { toast: false }),
        ]);
        if (list) {
          this.list = list;
          const ids = new Set(list.map((f) => f.id));
          for (const id of Object.keys(this.statuses)) if (!ids.has(id)) this.#forget(id);
        }
        for (const s of statuses ?? []) this.handle(s);
        this.loaded = true;
      } while (this.#again);
    })().finally(() => (this.#loading = null));
    return this.#loading;
  }

  /** Applies a `portForward` status event. */
  handle(s: PortForwardStatus) {
    const prev = this.statuses[s.id];
    this.statuses[s.id] = s;
    this.#sample(s, prev);
  }

  #sample(s: PortForwardStatus, prev: PortForwardStatus | undefined) {
    const now = performance.now();
    const last = this.#samples.get(s.id);
    // Counters reset when a forward (re)starts.
    const reset = !last || s.bytesIn < last.bytesIn || s.bytesOut < last.bytesOut || (prev && prev.state !== "active" && s.state === "active");
    if (s.state !== "active") {
      this.#samples.delete(s.id);
      if (this.rates[s.id]) delete this.rates[s.id];
      return;
    }
    this.#samples.set(s.id, { t: now, bytesIn: s.bytesIn, bytesOut: s.bytesOut });
    if (reset || !last) {
      this.rates[s.id] ??= { in: 0, out: 0 };
    } else {
      const dt = (now - last.t) / 1000;
      if (dt > 0.05) {
        const r = this.rates[s.id] ?? { in: 0, out: 0 };
        const inNow = (s.bytesIn - last.bytesIn) / dt;
        const outNow = (s.bytesOut - last.bytesOut) / dt;
        this.rates[s.id] = { in: r.in + ALPHA * (inNow - r.in), out: r.out + ALPHA * (outNow - r.out) };
      }
    }
    this.#ensureTicker();
  }

  /** Decays rates of forwards that went quiet (backends may only report changes). */
  #ensureTicker() {
    if (this.#ticker) return;
    this.#ticker = setInterval(() => {
      const now = performance.now();
      let any = false;
      for (const [id, sample] of this.#samples) {
        any = true;
        const r = this.rates[id];
        if (!r || (r.in === 0 && r.out === 0)) continue;
        if (now - sample.t > STALE_MS) {
          const next = { in: r.in * 0.4, out: r.out * 0.4 };
          this.rates[id] = next.in < 1 && next.out < 1 ? { in: 0, out: 0 } : next;
        }
      }
      if (!any) {
        clearInterval(this.#ticker);
        this.#ticker = undefined;
      }
    }, 1000);
  }

  #forget(id: string) {
    delete this.statuses[id];
    delete this.rates[id];
    delete this.pending[id];
    this.#samples.delete(id);
  }

  // ------------------------------------------------------------ actions

  async start(id: string): Promise<boolean> {
    const f = this.get(id);
    this.pending[id] = true;
    try {
      const s = await call((b) => b.forwardStart(id), { toast: false });
      this.handle(s);
      return s.state !== "failed";
    } catch (e) {
      const error = toCommandError(e);
      // Failures that the status already shows inline aren't toasted again.
      if (error.kind !== "cancelled" && this.status(id).state !== "failed") {
        toasts.error(error, `Couldn't start ${f ? `“${f.name}”` : "port forward"}`);
      }
      return false;
    } finally {
      delete this.pending[id];
    }
  }

  async stop(id: string): Promise<void> {
    this.pending[id] = true;
    try {
      await call((b) => b.forwardStop(id), { action: "Couldn't stop port forward" });
      // The status event follows; reflect the stop right away.
      if (this.status(id).state !== "stopped") this.handle(idle(id));
    } catch {
      // toasted
    } finally {
      delete this.pending[id];
    }
  }

  toggle(id: string) {
    if (isRunning(this.status(id))) void this.stop(id);
    else void this.start(id);
  }

  // CRUD rethrows (without toasting) so the editor can show errors inline.
  async create(draft: PortForwardDraft): Promise<PortForward> {
    const f = await call((b) => b.forwardCreate(draft), { toast: false });
    this.#upsert(f);
    return f;
  }

  async update(id: string, draft: PortForwardDraft): Promise<PortForward> {
    const f = await call((b) => b.forwardUpdate(id, draft), { toast: false });
    this.#upsert(f);
    return f;
  }

  async remove(id: string): Promise<boolean> {
    try {
      await call((b) => b.forwardDelete(id), { action: "Couldn't delete port forward" });
      this.list = this.list.filter((f) => f.id !== id);
      this.#forget(id);
      return true;
    } catch {
      return false;
    }
  }

  /** A name not used by any rule yet, based on `base`. */
  uniqueName(base: string): string {
    const taken = new Set(this.list.map((f) => f.name.toLowerCase()));
    if (!taken.has(base.toLowerCase())) return base;
    for (let i = 2; ; i++) if (!taken.has(`${base} ${i}`.toLowerCase())) return `${base} ${i}`;
  }

  hostName(id: HostId | null): string {
    return id ? (hosts.get(id)?.name ?? "Unknown host") : "this computer";
  }

  #upsert(f: PortForward) {
    const i = this.list.findIndex((x) => x.id === f.id);
    this.list = i === -1 ? [...this.list, f] : this.list.map((x) => (x.id === f.id ? f : x));
  }
}

export const forwards = new Forwards();
