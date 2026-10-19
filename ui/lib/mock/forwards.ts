// Mock port forwards and backend settings.
//
// Rules live in memory; starting one connects its host(s) through the normal
// mock connect flow (prompts included), then reports `active` and ticks
// traffic counters once a second. Scripted behaviour:
//   Grafana (bastion, local :3000)   fails: address already in use
//   Share dev server (staging-api)   fails: staging-api's auth fails
//   any running forward              a dropped connection (`drop-connection`
//                                    in a remote shell) -> reconnecting attempts;
//                                    hosts that need no prompts reconnect on their
//                                    own on attempt 2, others recover when you
//                                    reconnect them
//   explicit disconnect              stops the host's forwards
//   Production Postgres              auto-start (starts with the "app")

import type { Backend } from "../api";
import type {
  AppSettings,
  CommandError,
  CoreEvent,
  Host,
  HostId,
  PortForward,
  PortForwardDraft,
  PortForwardStatus,
} from "../types";

const BASTION = "cfg:bastion";
const DB = "cfg:db-internal";
const BUILD_MAC = "cfg:build-mac";
const PI = "cfg:raspberrypi";
const PROD_WEB = "cfg:prod-web-01";
const STAGING = "cfg:staging-api";
const WIN = "cfg:win-buildbox";

const remoteListen = (k: PortForward["kind"]) => k === "remote" || k === "remoteDynamic";
const socks = (k: PortForward["kind"]) => k === "dynamic" || k === "remoteDynamic";

const rule = (r: Partial<PortForward> & Pick<PortForward, "id" | "name" | "kind" | "hostId" | "bindPort">): PortForward => ({
  source: "app",
  bindAddress: null,
  targetHost: null,
  targetPort: null,
  viaHostId: null,
  autoStart: false,
  ...r,
});

const INITIAL: PortForward[] = [
  rule({
    id: "fwd:2b0c6f1e-postgres",
    name: "Production Postgres",
    kind: "local",
    hostId: BASTION,
    bindPort: 5432,
    targetHost: "db.internal",
    targetPort: 5432,
    autoStart: true,
  }),
  rule({
    id: "cfg:bastion:1",
    name: "LocalForward 8080",
    source: "sshConfig",
    kind: "local",
    hostId: BASTION,
    bindPort: 8080,
    targetHost: "intranet.example.net",
    targetPort: 80,
  }),
  rule({
    id: "fwd:8d41a7c2-grafana",
    name: "Grafana",
    kind: "local",
    hostId: BASTION,
    bindPort: 3000,
    targetHost: "grafana.internal",
    targetPort: 3000,
  }),
  rule({
    id: "fwd:51e2d9b0-redis",
    name: "Redis cache",
    kind: "local",
    hostId: DB,
    bindPort: 0,
    targetHost: "localhost",
    targetPort: 6379,
  }),
  rule({
    id: "fwd:c7a90e44-socks",
    name: "SOCKS proxy",
    kind: "dynamic",
    hostId: PROD_WEB,
    bindPort: 1080,
  }),
  rule({
    id: "fwd:5d1e9a70-home-socks",
    name: "Home network for staging",
    kind: "remoteDynamic",
    hostId: STAGING,
    bindPort: 1080,
  }),
  rule({
    id: "fwd:0f3b6d28-metrics",
    name: "Build agent metrics",
    kind: "remote",
    hostId: BASTION,
    bindPort: 9182,
    targetHost: "localhost",
    targetPort: 9182,
    viaHostId: WIN,
  }),
  rule({
    id: "fwd:9e7c2b15-pi-camera",
    name: "Pi camera on prod",
    kind: "remote",
    hostId: PROD_WEB,
    bindPort: 8080,
    targetHost: "localhost",
    targetPort: 80,
    viaHostId: PI,
  }),
  rule({
    id: "fwd:4a6e8f03-devserver",
    name: "Share dev server",
    kind: "remote",
    hostId: STAGING,
    bindAddress: "0.0.0.0",
    bindPort: 8080,
    targetHost: "localhost",
    targetPort: 5173,
  }),
  rule({
    id: "cfg:build-mac:1",
    name: "LocalForward 5900",
    source: "sshConfig",
    kind: "local",
    hostId: BUILD_MAC,
    bindPort: 5900,
    targetHost: "localhost",
    targetPort: 5900,
  }),
];

/** Local ports some other program on "this computer" already listens on. */
const BUSY_LOCAL_PORTS = new Set([3000, 631]);

export const DEFAULT_SETTINGS: AppSettings = {
  closeToTray: true,
  launchAtLogin: false,
  startMinimized: false,
  maxConcurrentTransfers: 3,
  tailscale: true,
};

export interface ForwardsDeps {
  /** Backend settings were saved. */
  onSettings?(settings: AppSettings): void;
  host(id: HostId): Host | undefined;
  isConnected(id: HostId): boolean;
  /** The mock `connect` (emits connection events, may prompt). */
  connect(id: HostId): Promise<void>;
  /** Hosts that connect without any prompt can be reconnected silently. */
  canReconnectSilently(id: HostId): boolean;
  emit(e: CoreEvent): void;
  err(kind: CommandError["kind"], message: string): CommandError;
  uuid(): string;
}

type ForwardMethods = Pick<
  Backend,
  | "forwardsList"
  | "forwardStatuses"
  | "forwardCreate"
  | "forwardUpdate"
  | "forwardDelete"
  | "forwardStart"
  | "forwardStop"
  | "settingsGet"
  | "settingsSet"
>;

interface Runtime {
  status: PortForwardStatus;
  /** Bumped on every start/stop so stale async steps bail out. */
  gen: number;
  traffic?: ReturnType<typeof setInterval>;
  retry?: ReturnType<typeof setTimeout>;
  /** Ephemeral port picked for bindPort 0. */
  autoPort?: number;
}

/** Bytes per second-ish and connection churn by service. */
function profile(f: PortForward): { rate: number; churn: number; maxOpen: number } {
  if (socks(f.kind)) return { rate: 180_000, churn: 0.55, maxOpen: 9 };
  switch (f.targetPort) {
    case 5432:
    case 3306:
      return { rate: 40_000, churn: 0.25, maxOpen: 4 };
    case 6379:
      return { rate: 9_000, churn: 0.15, maxOpen: 3 };
    case 5900:
      return { rate: 320_000, churn: 0.05, maxOpen: 1 };
    case 80:
    case 8080:
    case 5173:
      return { rate: 60_000, churn: 0.4, maxOpen: 6 };
    default:
      return { rate: 14_000, churn: 0.2, maxOpen: 3 };
  }
}

export function createForwards(deps: ForwardsDeps) {
  let rules: PortForward[] = structuredClone(INITIAL);
  let settings: AppSettings = { ...DEFAULT_SETTINGS };
  const runtimes = new Map<string, Runtime>();

  const stopped = (id: string): PortForwardStatus => ({
    id,
    state: "stopped",
    openConnections: 0,
    totalConnections: 0,
    bytesIn: 0,
    bytesOut: 0,
  });

  const rt = (id: string): Runtime => {
    let r = runtimes.get(id);
    if (!r) {
      r = { status: stopped(id), gen: 0 };
      runtimes.set(id, r);
    }
    return r;
  };

  const find = (id: string): PortForward => {
    const f = rules.find((r) => r.id === id);
    if (!f) throw deps.err("notFound", `no such port forward: ${id}`);
    return f;
  };

  const hostName = (id: HostId | null) => (id ? (deps.host(id)?.name ?? id) : "this computer");

  const publish = (r: Runtime) => deps.emit({ type: "portForward", ...structuredClone(r.status) });

  const set = (f: PortForward, patch: Partial<PortForwardStatus> & Pick<PortForwardStatus, "state">) => {
    const r = rt(f.id);
    // Rebuild so fields of the previous state (address, attempt…) don't linger.
    const { id, openConnections, totalConnections, bytesIn, bytesOut } = r.status;
    r.status = { id, openConnections, totalConnections, bytesIn, bytesOut, ...patch } as PortForwardStatus;
    publish(r);
  };

  const clearTimers = (r: Runtime) => {
    clearInterval(r.traffic);
    clearTimeout(r.retry);
    r.traffic = undefined;
    r.retry = undefined;
  };

  const loopback = (addr: string | null) => addr ?? "127.0.0.1";

  function address(f: PortForward, r: Runtime): string {
    if (remoteListen(f.kind)) return `${f.bindAddress ?? "localhost"}:${f.bindPort}`;
    if (f.bindPort === 0) r.autoPort ??= 49152 + Math.floor(Math.random() * 16000);
    return `${loopback(f.bindAddress)}:${f.bindPort || r.autoPort}`;
  }

  function startTraffic(f: PortForward, r: Runtime) {
    clearInterval(r.traffic);
    const p = profile(f);
    r.traffic = setInterval(() => {
      const s = r.status;
      if (s.state !== "active") return;
      let open = s.openConnections;
      let total = s.totalConnections;
      // Connections come and go.
      if (open < p.maxOpen && Math.random() < p.churn) {
        open++;
        total++;
      }
      if (open > 0 && Math.random() < p.churn * 0.7) open--;
      // Idle forwards see a trickle; busy ones bursts.
      const load = open === 0 ? 0 : open * (0.3 + Math.random() * 1.4);
      const down = Math.round(p.rate * load * (0.6 + Math.random() * 0.8));
      const up = Math.round(down * (0.05 + Math.random() * 0.2));
      r.status = { ...s, openConnections: open, totalConnections: total, bytesIn: s.bytesIn + down, bytesOut: s.bytesOut + up };
      publish(r);
    }, 1000);
  }

  function activate(f: PortForward, r: Runtime) {
    set(f, { state: "active", address: address(f, r) });
    startTraffic(f, r);
  }

  /** Hosts a forward needs connected. */
  const hostsOf = (f: PortForward): HostId[] => (f.viaHostId ? [f.hostId, f.viaHostId] : [f.hostId]);

  function bindError(f: PortForward): string | null {
    if (remoteListen(f.kind)) {
      if (f.bindPort < 1024 && deps.host(f.hostId)?.user !== "root") {
        return `remote port forwarding failed for listen port ${f.bindPort}: the server refused (ports below 1024 need root)`;
      }
      return null;
    }
    const port = f.bindPort;
    if (port === 0) return null;
    const addr = `${loopback(f.bindAddress)}:${port}`;
    if (BUSY_LOCAL_PORTS.has(port)) return `couldn't listen on ${addr}: address already in use`;
    const clash = rules.find(
      (o) =>
        o.id !== f.id &&
        !remoteListen(o.kind) &&
        o.bindPort === port &&
        ["active", "starting", "reconnecting"].includes(rt(o.id).status.state),
    );
    if (clash) return `couldn't listen on ${addr}: address already in use (by “${clash.name}”)`;
    return null;
  }

  async function start(id: string): Promise<PortForwardStatus> {
    const f = find(id);
    const r = rt(id);
    if (r.status.state === "active" || r.status.state === "starting") return structuredClone(r.status);
    clearTimers(r);
    const gen = ++r.gen;
    r.autoPort = undefined;
    r.status = stopped(id);
    set(f, { state: "starting" });
    try {
      for (const h of hostsOf(f)) {
        if (!deps.host(h)) throw deps.err("unknownHost", `unknown host: ${h}`);
        if (!deps.isConnected(h)) await deps.connect(h);
        if (gen !== r.gen) return structuredClone(r.status);
      }
    } catch (e) {
      if (gen !== r.gen) return structuredClone(r.status);
      const ce = e as CommandError;
      if (ce.kind === "cancelled") {
        set(f, { state: "stopped" });
        throw ce;
      }
      set(f, { state: "failed", message: ce.message });
      throw ce;
    }
    await new Promise((res) => setTimeout(res, 350 + Math.random() * 400));
    if (gen !== r.gen) return structuredClone(r.status);
    const bad = bindError(f);
    if (bad) {
      set(f, { state: "failed", message: bad });
      return structuredClone(r.status);
    }
    activate(f, r);
    return structuredClone(r.status);
  }

  function stop(id: string) {
    const f = find(id);
    const r = rt(id);
    r.gen++;
    clearTimers(r);
    if (r.status.state === "stopped") return;
    r.status = stopped(id);
    set(f, { state: "stopped" });
  }

  function scheduleRetry(f: PortForward, r: Runtime, attempt: number, lostHost: HostId) {
    const gen = r.gen;
    const message = `connection to ${hostName(lostHost)} lost`;
    set(f, { state: "reconnecting", attempt, message, openConnections: 0 });
    r.retry = setTimeout(async () => {
      if (gen !== r.gen || r.status.state !== "reconnecting") return;
      const missing = hostsOf(f).filter((h) => !deps.isConnected(h));
      if (missing.length === 0) {
        activate(f, r);
        return;
      }
      if (attempt >= 2 && missing.every((h) => deps.canReconnectSilently(h))) {
        try {
          for (const h of missing) await deps.connect(h);
          // hostConnected() activates it.
          return;
        } catch {
          // fall through to the next attempt
        }
      }
      if (gen !== r.gen) return;
      if (attempt >= 6) {
        set(f, { state: "failed", message: `gave up reconnecting after ${attempt} attempts: ${message}` });
        return;
      }
      scheduleRetry(f, r, attempt + 1, lostHost);
    }, 2500);
  }

  // Auto-start rules, as the app does at launch.
  setTimeout(() => {
    for (const f of rules) if (f.autoStart) void start(f.id).catch(() => {});
  }, 500);

  function validate(d: PortForwardDraft, selfId: string | null): PortForward {
    const name = d.name.trim();
    if (!name) throw deps.err("other", "name must not be empty");
    if (!deps.host(d.hostId)) throw deps.err("unknownHost", `unknown host: ${d.hostId}`);
    const port = (p: number | null, what: string, allowZero: boolean) => {
      if (p === null || !Number.isInteger(p) || p < (allowZero ? 0 : 1) || p > 65535)
        throw deps.err("other", `${what} must be between ${allowZero ? 0 : 1} and 65535`);
      return p;
    };
    port(d.bindPort, "bind port", !remoteListen(d.kind));
    if (!socks(d.kind)) {
      if (!d.targetHost?.trim()) throw deps.err("other", "target host must not be empty");
      port(d.targetPort, "target port", false);
    }
    if (d.viaHostId) {
      if (!remoteListen(d.kind)) throw deps.err("other", "only forwards that listen on a server can connect out through another host");
      if (d.viaHostId === d.hostId) throw deps.err("other", "a forward can't connect from the host it listens on");
      if (!deps.host(d.viaHostId)) throw deps.err("unknownHost", `unknown host: ${d.viaHostId}`);
    }
    const clash = rules.find((r) => r.id !== selfId && r.name.toLowerCase() === name.toLowerCase());
    if (clash) throw deps.err("alreadyExists", `a port forward named "${name}" already exists`);
    return {
      id: selfId ?? `fwd:${deps.uuid()}`,
      name,
      source: "app",
      kind: d.kind,
      hostId: d.hostId,
      bindAddress: d.bindAddress?.trim() || null,
      bindPort: d.bindPort,
      targetHost: socks(d.kind) ? null : (d.targetHost?.trim() ?? null),
      targetPort: socks(d.kind) ? null : d.targetPort,
      viaHostId: remoteListen(d.kind) ? d.viaHostId : null,
      autoStart: d.autoStart,
    };
  }

  const methods: ForwardMethods = {
    async forwardsList() {
      return structuredClone(rules);
    },
    async forwardStatuses() {
      return [...runtimes.values()].filter((r) => r.status.state !== "stopped").map((r) => structuredClone(r.status));
    },
    async forwardCreate(draft) {
      const f = validate(draft, null);
      rules = [...rules, f];
      deps.emit({ type: "portForwardsChanged" });
      return structuredClone(f);
    },
    async forwardUpdate(id, draft) {
      const old = find(id);
      if (old.source !== "app") throw deps.err("readOnlyHost", "rules from ~/.ssh/config are read-only");
      const f = validate(draft, id);
      rules = rules.map((r) => (r.id === id ? f : r));
      deps.emit({ type: "portForwardsChanged" });
      const state = rt(id).status.state;
      if (state !== "stopped" && state !== "failed") {
        stop(id);
        void start(id).catch(() => {});
      }
      return structuredClone(f);
    },
    async forwardDelete(id) {
      const f = find(id);
      if (f.source !== "app") throw deps.err("readOnlyHost", "rules from ~/.ssh/config are read-only");
      stop(id);
      runtimes.delete(id);
      rules = rules.filter((r) => r.id !== id);
      deps.emit({ type: "portForwardsChanged" });
    },
    forwardStart: start,
    async forwardStop(id) {
      stop(id);
    },
    async settingsGet() {
      return { ...settings };
    },
    async settingsSet(next) {
      const n = Math.round(Number(next.maxConcurrentTransfers));
      settings = {
        closeToTray: !!next.closeToTray,
        launchAtLogin: !!next.launchAtLogin,
        startMinimized: !!next.launchAtLogin && !!next.startMinimized,
        maxConcurrentTransfers: Number.isFinite(n) ? Math.min(8, Math.max(1, n)) : settings.maxConcurrentTransfers,
        tailscale: next.tailscale !== false,
      };
      deps.emit({ type: "settingsChanged", ...settings });
      deps.onSettings?.(settings);
      return { ...settings };
    },
  };

  return {
    methods,
    /** A host finished connecting: forwards waiting on it come back. */
    hostConnected(hostId: HostId) {
      for (const f of rules) {
        const r = runtimes.get(f.id);
        if (r?.status.state !== "reconnecting" || !hostsOf(f).includes(hostId)) continue;
        if (hostsOf(f).every((h) => deps.isConnected(h))) {
          clearTimeout(r.retry);
          activate(f, r);
        }
      }
    },
    /** The connection dropped (network): running forwards try to reconnect. */
    hostLost(hostId: HostId) {
      for (const f of rules) {
        const r = runtimes.get(f.id);
        if (!r || !hostsOf(f).includes(hostId)) continue;
        if (r.status.state !== "active" && r.status.state !== "starting") continue;
        clearTimers(r);
        scheduleRetry(f, r, 1, hostId);
      }
    },
    /** The user disconnected the host: its forwards stop. */
    hostDisconnected(hostId: HostId) {
      for (const f of rules) if (hostsOf(f).includes(hostId) && runtimes.get(f.id)?.status.state !== "stopped") stop(f.id);
    },
    hostDeleted(hostId: HostId) {
      const gone = rules.filter((f) => hostsOf(f).includes(hostId));
      if (!gone.length) return;
      for (const f of gone) {
        if (runtimes.has(f.id)) stop(f.id);
        runtimes.delete(f.id);
      }
      rules = rules.filter((f) => !gone.includes(f));
      deps.emit({ type: "portForwardsChanged" });
    },
  };
}
