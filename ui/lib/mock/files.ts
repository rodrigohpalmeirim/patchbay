// Mock implementations of the fs*, transfer* and search* backend commands on
// top of the in-memory filesystem (vfs.ts / trees.ts).
//
// Transfers: queued -> scanning -> running -> done, at most 3 at a time
// (background ones start at once and don't count), progress events every
// ~200 ms; `maxBytes` fails them after scanning. Conflicts raise `fileConflict` prompts. A
// file named *big-dataset* fails once at ~40% (resumable); retry continues.
// Disconnecting a host fails its transfers as resumable. `transferList`
// returns transfers in creation order (oldest first).
//
// Search: breadth-first walk emitting `search` batches every ~120-400 ms.

import type { Backend } from "../api";
import type {
  CommandError,
  ConflictResolution,
  CoreEvent,
  FileEntry,
  FileRef,
  Host,
  HostId,
  Prompt,
  PromptResponse,
  TransferInfo,
  TransferState,
} from "../types";
import { readContent } from "./content";
import { buildMachine } from "./trees";
import {
  basename,
  canChmod,
  canWrite,
  dirname,
  fsErr,
  inWritableRoot,
  join,
  kids,
  MB,
  normalize,
  resolve,
  toEntry,
  tryResolve,
  type DirNode,
  type FileNode,
  type Machine,
  type Node,
} from "./vfs";

export interface FilesDeps {
  host(id: HostId): Host | undefined;
  hostByName(name: string): Host | undefined;
  isConnected(id: HostId): boolean;
  emit(e: CoreEvent): void;
  /** Shows a prompt; rejects if `signal` aborts (the prompt is then closed). */
  ask(hostId: HostId | null, prompt: Prompt, signal: AbortSignal): Promise<PromptResponse>;
}

type FileMethods = Pick<
  Backend,
  | "fsHome"
  | "fsList"
  | "fsStat"
  | "fsMkdir"
  | "fsCreateFile"
  | "fsRename"
  | "fsDelete"
  | "fsChmod"
  | "fsChown"
  | "fsRead"
  | "transferStart"
  | "transferList"
  | "transferPause"
  | "transferResume"
  | "transferCancel"
  | "transferRetry"
  | "transferClearFinished"
  | "searchStart"
  | "searchCancel"
>;

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));
const rand = (lo: number, hi: number) => lo + Math.random() * (hi - lo);
const nowSecs = () => Math.floor(Date.now() / 1000);
const id = () =>
  typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : Math.random().toString(16).slice(2) + Date.now().toString(16);

function err(kind: CommandError["kind"], message: string): CommandError {
  return { kind, message };
}

// -------------------------------------------------------------- transfers

type TransferBase = Omit<TransferInfo, "state" | "message" | "resumable">;

interface PlanItem {
  /** Path relative to the destination dir. */
  rel: string[];
  kind: "dir" | "file";
  node: FileNode | null;
  size: number;
}

interface Job {
  base: TransferBase;
  state: TransferState;
  sources: FileRef[];
  plan: PlanItem[] | null;
  index: number;
  fileDone: number;
  /** plan[index] passed the conflict check. */
  checked: boolean;
  /** Destination name for plan[index] when "keep both" renamed it. */
  targetName: string | null;
  /** Node being written for plan[index]. */
  target: { dir: DirNode; node: FileNode } | null;
  policy: ConflictResolution | null;
  prompt: AbortController | null;
  gen: number;
  live: boolean;
  failedOnce: boolean;
  speed: number;
  maxBytes: number | null;
}

const FINISHED = new Set(["done", "failed", "cancelled"]);

export function createFiles(deps: FilesDeps): {
  methods: FileMethods;
  hostDisconnected(id: HostId): void;
  /** Creates a directory and its missing parents. */
  ensureDir(hostId: HostId | null, path: string): void;
  /** Filesystem of a machine (remote hosts must be connected); used by the mock shell. */
  machine(hostId: HostId | null): Machine;
} {
  const machines = new Map<string, Machine>();
  const jobs: Job[] = [];
  const searches = new Map<string, { cancelled: boolean }>();

  // --------------------------------------------------------------- access

  function hostName(hostId: HostId | null): string {
    return hostId === null ? "this computer" : (deps.host(hostId)?.name ?? hostId);
  }

  /** The machine for `hostId`; remote hosts must be connected. */
  function machine(hostId: HostId | null): Machine {
    if (hostId === null) {
      let m = machines.get("local");
      if (!m) machines.set("local", (m = buildMachine(null)));
      return m;
    }
    const host = deps.host(hostId);
    if (!host) throw err("unknownHost", `unknown host: ${hostId}`);
    if (!deps.isConnected(hostId)) throw err("notConnected", `not connected to ${host.name}`);
    let m = machines.get(hostId);
    if (!m) machines.set(hostId, (m = buildMachine(host)));
    return m;
  }

  async function latency(m: Machine, slow = false) {
    await sleep(m.remote ? rand(60, 250) : rand(10, 40));
    if (slow) await sleep(rand(320, 420));
  }

  function dirAt(m: Machine, path: string): DirNode {
    const r = resolve(m, path, true);
    if (r.node.kind !== "dir") throw fsErr(m, "notDir", path);
    return r.node;
  }

  function newDir(m: Machine, name: string): DirNode {
    return {
      kind: "dir",
      name,
      modified: nowSecs(),
      permissions: 0o755,
      owner: m.windows ? null : m.user,
      group: m.windows ? null : m.group,
      children: new Map(),
      lazy: null,
      denied: false,
      slow: false,
    };
  }

  function newFile(m: Machine, name: string, src: FileNode | null, size: number): FileNode {
    return {
      kind: "file",
      name,
      modified: nowSecs(),
      permissions: src ? src.permissions & 0o777 : 0o644,
      owner: m.windows ? null : m.user,
      group: m.windows ? null : m.group,
      size,
      content: src ? src.content : { t: "bytes", data: new Uint8Array() },
    };
  }

  /** Parent dir of `path`, checked for write access. */
  function writableParent(m: Machine, path: string): DirNode {
    const parentPath = dirname(path);
    const parent = dirAt(m, parentPath);
    if (!canWrite(m, parent, parentPath)) throw fsErr(m, "denied", path);
    return parent;
  }

  function create(m: Machine, path: string, node: Node) {
    const p = normalize(path);
    if (p === "/") throw fsErr(m, "exists", p);
    const parent = writableParent(m, p);
    const name = basename(p);
    if (kids(parent).has(name)) throw fsErr(m, "exists", p);
    node.name = name;
    kids(parent).set(name, node);
    parent.modified = nowSecs();
  }

  function forEachNode(node: Node, path: string, recursive: boolean, fn: (n: Node, path: string) => void) {
    fn(node, path);
    if (recursive && node.kind === "dir" && !node.denied) {
      for (const c of kids(node).values()) forEachNode(c, join(path, c.name), true, fn);
    }
  }

  function uniqueName(dir: DirNode, name: string): string {
    const dot = name.lastIndexOf(".");
    const stem = dot > 0 ? name.slice(0, dot) : name;
    const ext = dot > 0 ? name.slice(dot) : "";
    for (let i = 1; ; i++) {
      const candidate = `${stem} (${i})${ext}`;
      if (!kids(dir).has(candidate)) return candidate;
    }
  }

  // ------------------------------------------------------------ transfers

  const info = (job: Job): TransferInfo => ({ ...job.base, ...job.state }) as TransferInfo;
  const emitJob = (job: Job) => deps.emit({ type: "transfer", ...info(job) });

  function setState(job: Job, state: TransferState) {
    job.state = state;
    job.base.doneBytes = Math.round(job.base.doneBytes);
    if (FINISHED.has(state.state)) {
      job.base.finishedAt = Date.now();
      job.base.bytesPerSec = 0;
      job.base.currentFile = null;
    } else {
      job.base.finishedAt = null;
    }
    if (state.state !== "running") job.base.bytesPerSec = 0;
    emitJob(job);
  }

  function hostsOf(job: Job): (HostId | null)[] {
    return [job.base.sourceHost, job.base.destination.hostId];
  }

  function stop(job: Job) {
    job.gen++;
    job.live = false;
    job.prompt?.abort();
    job.prompt = null;
  }

  function fail(job: Job, message: string, resumable: boolean) {
    stop(job);
    setState(job, { state: "failed", message, resumable });
    pump();
  }

  /** Starts (or un-queues) transfers while fewer than 3 are active. */
  function pump() {
    let active = jobs.filter(
      (j) => j.live && !j.base.background && j.state.state !== "paused" && j.state.state !== "queued",
    ).length;
    for (const job of jobs) {
      if (job.state.state !== "queued") continue;
      if (!job.base.background) {
        if (active >= 3) continue;
        active++;
      }
      if (job.live) setState(job, { state: "running" });
      else start(job);
    }
  }

  function start(job: Job) {
    const my = ++job.gen;
    job.live = true;
    run(job, () => job.gen === my)
      .catch((e: unknown) => {
        if (job.gen !== my) return;
        const ce = e as Partial<CommandError>;
        const lost = ce.kind === "notConnected";
        const gone = hostsOf(job).find((h) => h !== null && !deps.isConnected(h)) ?? job.base.destination.hostId;
        fail(job, lost ? `connection to ${hostName(gone)} lost` : (ce.message ?? String(e)), lost);
      })
      .finally(() => {
        if (job.gen === my) job.live = false;
        pump();
      });
  }

  /** Flattens the sources into dirs (pre-order) and files. */
  function scan(job: Job) {
    const m = machine(job.base.sourceHost);
    const plan: PlanItem[] = [];
    let skipped = 0;
    const seen = new Set<string>();
    const walk = (path: string, rel: string[], depth: number) => {
      const r = tryResolve(m, path, true);
      if (!r) {
        skipped++;
        return;
      }
      if (r.node.kind === "file") {
        plan.push({ rel, kind: "file", node: r.node, size: r.node.size });
      } else if (r.node.kind === "dir") {
        if (r.node.denied || seen.has(r.real) || depth > 32) {
          skipped++;
          return;
        }
        seen.add(r.real);
        plan.push({ rel, kind: "dir", node: null, size: 0 });
        for (const c of kids(r.node).values()) walk(join(r.real, c.name), [...rel, c.name], depth + 1);
      } else {
        skipped++;
      }
    };
    for (const s of job.sources) walk(normalize(s.path), [basename(s.path) || "root"], 0);
    job.plan = plan;
    job.index = 0;
    job.fileDone = 0;
    job.checked = false;
    job.target = null;
    job.base.doneBytes = 0;
    job.base.doneFiles = 0;
    job.base.skippedFiles = skipped;
    job.base.totalBytes = plan.reduce((n, i) => n + i.size, 0);
    job.base.totalFiles = plan.filter((i) => i.kind === "file").length;
  }

  function destPathOf(job: Job, item: PlanItem, name = item.rel[item.rel.length - 1]): string {
    return join(normalize(job.base.destination.path), [...item.rel.slice(0, -1), name].join("/"));
  }

  /** Skips the current item, counting its bytes as done. */
  function skipCurrent(job: Job, item: PlanItem) {
    job.base.skippedFiles++;
    job.base.doneBytes += item.size - job.fileDone;
    job.index++;
    job.fileDone = 0;
    job.checked = false;
    job.targetName = null;
    job.target = null;
  }

  /**
   * Creates directories and clears non-conflicting files up to the next
   * file that's ready to copy. Returns the existing node if plan[index]
   * needs a conflict decision.
   */
  function prepare(job: Job, dm: Machine): Node | null {
    const plan = job.plan ?? [];
    while (job.index < plan.length) {
      const item = plan[job.index];
      const path = destPathOf(job, item);
      const parentPath = dirname(path);
      const parent = tryResolve(dm, parentPath, true);
      if (!parent || parent.node.kind !== "dir") {
        // Parent was skipped (e.g. name clash with a file).
        skipCurrent(job, item);
        continue;
      }
      const dir = parent.node;
      if (!canWrite(dm, dir, parentPath)) throw fsErr(dm, "denied", parentPath);
      const existing = kids(dir).get(basename(path)) ?? null;
      if (item.kind === "dir") {
        if (!existing) kids(dir).set(basename(path), newDir(dm, basename(path)));
        else if (existing.kind !== "dir") job.base.skippedFiles++;
        job.index++;
        continue;
      }
      if (job.checked) return null;
      if (!existing) {
        job.checked = true;
        return null;
      }
      if (existing.kind === "dir") {
        skipCurrent(job, item);
        continue;
      }
      if (job.policy) {
        applyResolution(job, item, existing, job.policy, dir);
        continue;
      }
      return existing;
    }
    return null;
  }

  function applyResolution(job: Job, item: PlanItem, existing: Node, res: ConflictResolution, dir: DirNode) {
    const src = item.node;
    switch (res) {
      case "skip":
        skipCurrent(job, item);
        return;
      case "overwriteIfNewer":
        if (!src || src.modified <= existing.modified) {
          skipCurrent(job, item);
          return;
        }
        break;
      case "rename":
        job.targetName = uniqueName(dir, item.rel[item.rel.length - 1]);
        break;
      case "resume":
        if (existing.kind === "file" && existing.size < item.size) {
          job.fileDone = existing.size;
          job.base.doneBytes += existing.size;
          job.target = { dir, node: existing };
        }
        break;
      case "overwrite":
        break;
    }
    job.checked = true;
  }

  async function askConflict(job: Job, item: PlanItem, existing: Node): Promise<ConflictResolution | null> {
    const path = destPathOf(job, item);
    job.base.currentFile = item.rel.join("/");
    setState(job, { state: "waitingForUser" });
    const ctrl = new AbortController();
    job.prompt = ctrl;
    const existingSize = existing.kind === "file" ? existing.size : 0;
    let response: PromptResponse;
    try {
      response = await deps.ask(
        job.base.destination.hostId ?? job.base.sourceHost,
        {
          kind: "fileConflict",
          transferId: job.base.id,
          path,
          source: { size: item.size, modified: item.node?.modified ?? null },
          existing: { size: existingSize, modified: existing.modified },
          canResume: existing.kind === "file" && existingSize < item.size,
        },
        ctrl.signal,
      );
    } catch {
      return null;
    } finally {
      if (job.prompt === ctrl) job.prompt = null;
    }
    if (response.kind !== "conflict") {
      cancel(job);
      return null;
    }
    if (response.applyToAll) job.policy = response.resolution;
    return response.resolution;
  }

  function finishFile(job: Job) {
    job.base.doneFiles++;
    job.index++;
    job.fileDone = 0;
    job.checked = false;
    job.targetName = null;
    job.target = null;
  }

  /** Writes `job.fileDone` bytes of plan[index] to the destination. */
  function writeProgress(job: Job, dm: Machine, item: PlanItem) {
    if (!job.target) {
      const path = destPathOf(job, item, job.targetName ?? undefined);
      const dir = dirAt(dm, dirname(path));
      job.target = { dir, node: newFile(dm, basename(path), item.node, 0) };
    }
    const { dir, node } = job.target;
    node.size = Math.floor(job.fileDone);
    node.modified = nowSecs();
    node.content = item.node?.content ?? null;
    if (job.fileDone >= item.size && item.node) node.permissions = item.node.permissions & 0o777;
    kids(dir).set(node.name, node);
  }

  async function run(job: Job, alive: () => boolean) {
    setState(job, { state: "scanning" });
    await sleep(job.base.background ? rand(80, 200) : rand(300, 900));
    if (!alive()) return;
    machine(job.base.sourceHost);
    if (!job.plan) scan(job);
    if (job.maxBytes != null && (job.base.totalBytes ?? 0) > job.maxBytes) {
      return fail(job, `${job.base.label} is ${job.base.totalBytes} bytes, more than the limit of ${job.maxBytes} bytes`, false);
    }
    const dm = machine(job.base.destination.hostId);
    const destDir = normalize(job.base.destination.path);
    const dir = tryResolve(dm, destDir, true);
    if (!dir) return fail(job, `no such file or directory: ${destDir}`, false);
    if (dir.node.kind !== "dir" || !canWrite(dm, dir.node, destDir) || !inWritableRoot(dm, destDir)) {
      return fail(job, `permission denied: ${destDir}`, false);
    }
    const remoteCount = hostsOf(job).filter((h) => h !== null).length;
    const lan = hostsOf(job).every((h) => h === null || machine(h).lan);
    const [lo, hi] = remoteCount === 0 ? [180, 420] : remoteCount === 2 ? [3, 10] : lan ? [40, 110] : [6, 25];
    job.speed = Math.max(rand(lo, hi) * MB, (job.base.totalBytes ?? 0) / 15);
    if (job.state.state === "scanning") setState(job, { state: "running" });

    let last = performance.now();
    const plan = job.plan ?? [];
    for (;;) {
      if (!alive()) return;
      if (job.state.state === "paused" || job.state.state === "queued") {
        await sleep(150);
        last = performance.now();
        continue;
      }
      for (const h of hostsOf(job)) machine(h); // throws notConnected if a host dropped
      const conflict = prepare(job, dm);
      if (job.index >= plan.length) break;
      if (conflict) {
        const item = plan[job.index];
        const res = await askConflict(job, item, conflict);
        if (!alive() || res === null) return;
        const parent = dirAt(dm, dirname(destPathOf(job, item)));
        applyResolution(job, item, conflict, res, parent);
        if (job.state.state === "waitingForUser") setState(job, { state: "running" });
        last = performance.now();
        continue;
      }

      await sleep(rand(150, 250));
      if (!alive()) return;
      if (job.state.state !== "running") continue;
      const t = performance.now();
      const speed = job.speed * rand(0.82, 1.15);
      let budget = (speed * (t - last)) / 1000;
      last = t;
      while (budget > 0) {
        const c = prepare(job, dm);
        if (c || job.index >= plan.length) break;
        const item = plan[job.index];
        let step = Math.min(item.size - job.fileDone, Math.ceil(budget));
        const failAt = item.size * 0.4;
        const doomed = item.rel[item.rel.length - 1].includes("big-dataset") && !job.failedOnce;
        if (doomed && job.fileDone + step >= failAt) {
          step = Math.max(0, Math.floor(failAt) - job.fileDone);
          job.fileDone += step;
          job.base.doneBytes += step;
          writeProgress(job, dm, item);
          job.failedOnce = true;
          return fail(job, `connection reset by peer while writing ${item.rel[item.rel.length - 1]}`, true);
        }
        job.fileDone += step;
        job.base.doneBytes += step;
        budget -= step;
        job.base.currentFile = item.rel.join("/");
        writeProgress(job, dm, item);
        if (job.fileDone >= item.size) finishFile(job);
      }
      job.base.doneBytes = Math.min(Math.round(job.base.doneBytes), job.base.totalBytes ?? Infinity);
      job.base.bytesPerSec = Math.round(speed);
      emitJob(job);
    }
    job.base.doneBytes = job.base.totalBytes ?? job.base.doneBytes;
    stop(job);
    setState(job, { state: "done" });
  }

  function cancel(job: Job) {
    if (FINISHED.has(job.state.state)) return;
    stop(job);
    setState(job, { state: "cancelled" });
    pump();
  }

  function findJob(jobId: string): Job {
    const job = jobs.find((j) => j.base.id === jobId);
    if (!job) throw err("other", `unknown transfer: ${jobId}`);
    return job;
  }

  function seedHistory() {
    const today = new Date();
    today.setHours(9, 12, 0, 0);
    const at = (h: number) => Math.min(Date.now() - 60_000, today.getTime() + h * 3600_000);
    const mk = (
      host: string,
      label: string,
      sourceHost: HostId | null,
      sources: string[],
      destination: FileRef,
      bytes: [number, number],
      files: [number, number],
      created: number,
      state: TransferState,
    ) => {
      if (!deps.hostByName(host)) return;
      jobs.push({
        base: {
          id: id(),
          label,
          background: false,
          sourceHost,
          destination,
          totalBytes: Math.round(bytes[1]),
          doneBytes: Math.round(bytes[0]),
          totalFiles: files[1],
          doneFiles: files[0],
          skippedFiles: 0,
          currentFile: null,
          bytesPerSec: 0,
          createdAt: created,
          finishedAt: created + 9_000,
        },
        state,
        sources: sources.map((path) => ({ hostId: sourceHost, path })),
        plan: null,
        index: 0,
        fileDone: 0,
        checked: false,
        targetName: null,
        target: null,
        policy: null,
        prompt: null,
        gen: 0,
        live: false,
        failedOnce: true,
        speed: 0,
        maxBytes: null,
      });
    };
    const bastion = deps.hostByName("bastion")?.id ?? null;
    const pi = deps.hostByName("raspberrypi")?.id ?? null;
    const web = deps.hostByName("prod-web-01")?.id ?? null;
    mk("bastion", "access.log", bastion, ["/var/log/nginx/access.log"], { hostId: null, path: "/home/rodrigo/Downloads" }, [48 * MB, 48 * MB], [1, 1], at(0), { state: "done" });
    mk("raspberrypi", "vacation-2025", null, ["/home/rodrigo/Pictures/vacation-2025"], { hostId: pi, path: "/home/pi/photos" }, [21 * MB, 49 * MB], [6, 14], at(1.5), { state: "cancelled" });
    mk("prod-web-01", "patchbay_0.1.0_amd64.deb", null, ["/home/rodrigo/Downloads/patchbay_0.1.0_amd64.deb"], { hostId: web, path: "/home/deploy" }, [5.1 * MB, 14.2 * MB], [0, 1], at(2.2), {
      state: "failed",
      message: "connection to prod-web-01 lost",
      resumable: true,
    });
  }
  seedHistory();

  // --------------------------------------------------------------- search

  function globToRegex(q: string): RegExp {
    const src = q.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*").replace(/\?/g, ".");
    return new RegExp(`^${src}$`, "i");
  }

  async function runSearch(searchId: string, hostId: HostId | null, m: Machine, root: string, query: string) {
    const state = searches.get(searchId);
    const send = (entries: FileEntry[], done: boolean, error: string | null = null) => {
      if (!state?.cancelled) deps.emit({ type: "search", searchId, entries, done, error });
    };
    const q = query.trim();
    const match: (name: string) => boolean = /[*?]/.test(q)
      ? ((re) => (name: string) => re.test(name))(globToRegex(q))
      : (name) => name.toLowerCase().includes(q.toLowerCase());
    const rootNode = tryResolve(m, root, true);
    await sleep(rand(120, 200));
    if (!rootNode || rootNode.node.kind !== "dir" || rootNode.node.denied) {
      send([], true, `permission denied: ${root}`);
      searches.delete(searchId);
      return;
    }
    const budget = m.exec ? 400 : 60;
    const pace: [number, number] = m.exec ? [120, 250] : [250, 400];
    const queue: { path: string; dir: DirNode }[] = [{ path: root, dir: rootNode.node }];
    while (queue.length) {
      if (state?.cancelled) return;
      if (hostId !== null && !deps.isConnected(hostId)) {
        send([], true, `connection to ${hostName(hostId)} lost`);
        searches.delete(searchId);
        return;
      }
      const batch: FileEntry[] = [];
      let processed = 0;
      while (queue.length && processed < budget) {
        const { path, dir } = queue.shift()!;
        if (dir.denied) continue;
        for (const child of kids(dir).values()) {
          processed++;
          const childPath = join(path, child.name);
          if (q && match(child.name)) batch.push(toEntry(m, childPath, child));
          if (child.kind === "dir") queue.push({ path: childPath, dir: child });
        }
      }
      if (batch.length) send(batch, false);
      await sleep(rand(...pace));
    }
    send([], true);
    searches.delete(searchId);
  }

  // -------------------------------------------------------------- methods

  const methods: FileMethods = {
    async fsHome(hostId) {
      const m = machine(hostId);
      await latency(m);
      return m.home;
    },

    async fsList(hostId, path) {
      const m = machine(hostId);
      const p = normalize(path);
      const r = tryResolve(m, p, true);
      await latency(m, r?.node.kind === "dir" && r.node.slow);
      const dir = dirAt(m, p);
      if (dir.denied) throw fsErr(m, "denied", p);
      const entries = [...kids(dir).values()].map((n) => toEntry(m, join(p, n.name), n));
      return { path: p, parent: p === "/" ? null : dirname(p), entries };
    },

    async fsStat(hostId, path) {
      const m = machine(hostId);
      await latency(m);
      const p = normalize(path);
      return toEntry(m, p, resolve(m, p, false).node);
    },

    async fsMkdir(hostId, path) {
      const m = machine(hostId);
      await latency(m);
      create(m, path, newDir(m, basename(path)));
    },

    async fsCreateFile(hostId, path) {
      const m = machine(hostId);
      await latency(m);
      create(m, path, newFile(m, basename(path), null, 0));
    },

    async fsRename(hostId, from, to) {
      const m = machine(hostId);
      await latency(m);
      const f = normalize(from);
      const t = normalize(to);
      if (f === t) return;
      if (f === "/") throw fsErr(m, "denied", f);
      const node = resolve(m, f, false).node;
      if (node.kind === "dir" && (t + "/").startsWith(f + "/")) {
        throw fsErr(m, "invalid", t, "cannot move a directory into itself");
      }
      const src = writableParent(m, f);
      const dst = writableParent(m, t);
      if (kids(dst).has(basename(t))) throw fsErr(m, "exists", t);
      kids(src).delete(node.name);
      node.name = basename(t);
      kids(dst).set(node.name, node);
      src.modified = dst.modified = nowSecs();
    },

    async fsDelete(hostId, paths) {
      const m = machine(hostId);
      await latency(m);
      for (const path of paths) {
        const p = normalize(path);
        if (p === "/") throw fsErr(m, "denied", p);
        const node = resolve(m, p, false).node;
        const parent = writableParent(m, p);
        kids(parent).delete(node.name);
        parent.modified = nowSecs();
      }
    },

    async fsChmod(hostId, paths, mode, recursive) {
      const m = machine(hostId);
      await latency(m);
      for (const path of paths) {
        const p = normalize(path);
        const r = resolve(m, p, true);
        if (!canChmod(m, r.node, p)) throw fsErr(m, "denied", p, "operation not permitted");
        forEachNode(r.node, r.real, recursive, (n, np) => {
          if (n.kind !== "symlink" && canChmod(m, n, np)) n.permissions = mode & 0o7777;
        });
      }
    },

    async fsChown(hostId, paths, owner, group, recursive) {
      const m = machine(hostId);
      await latency(m);
      const numeric = (s: string | null) => s === null || /^\d+$/.test(s.trim());
      if (!m.exec && (!numeric(owner) || !numeric(group))) {
        throw err("unsupported", "changing owner by name requires a remote shell; use a numeric uid/gid");
      }
      for (const path of paths) {
        const p = normalize(path);
        if (!inWritableRoot(m, p)) throw fsErr(m, "denied", p, "operation not permitted");
        const r = resolve(m, p, true);
        forEachNode(r.node, r.real, recursive, (n) => {
          if (owner !== null && owner.trim()) n.owner = owner.trim();
          if (group !== null && group.trim()) n.group = group.trim();
        });
      }
    },

    async fsRead(hostId, path, maxBytes) {
      const m = machine(hostId);
      await latency(m);
      const p = normalize(path);
      const r = resolve(m, p, true);
      if (r.node.kind === "dir") throw fsErr(m, "isDir", p);
      if (r.node.kind !== "file") throw fsErr(m, "invalid", p);
      const data = await readContent(r.node.content, r.node.name, r.node.size, maxBytes);
      // Simulate transfer time for remote reads (~8 MB/s).
      if (m.remote) await sleep(Math.min(1500, data.length / 8000));
      return data;
    },

    async transferStart(request) {
      const { sources, destination } = request;
      if (sources.length === 0) throw err("other", "nothing to transfer");
      const sourceHost = sources[0].hostId;
      if (sources.some((s) => s.hostId !== sourceHost)) throw err("other", "all sources must be on the same machine");
      const sm = machine(sourceHost);
      const dm = machine(destination.hostId);
      const dest = normalize(destination.path);
      dirAt(dm, dest);
      for (const s of sources) {
        const p = normalize(s.path);
        resolve(sm, p, false);
        const r = tryResolve(sm, p, true);
        if (sourceHost === destination.hostId && r?.node.kind === "dir" && (dest + "/").startsWith(r.real + "/")) {
          throw err(sm.remote ? "sftp" : "io", `cannot copy a directory into itself: ${p}`);
        }
      }
      const job: Job = {
        base: {
          id: id(),
          label: sources.length === 1 ? basename(sources[0].path) || sources[0].path : `${sources.length} items`,
          background: request.background ?? false,
          sourceHost,
          destination: { hostId: destination.hostId, path: dest },
          totalBytes: null,
          doneBytes: 0,
          totalFiles: null,
          doneFiles: 0,
          skippedFiles: 0,
          currentFile: null,
          bytesPerSec: 0,
          createdAt: Date.now(),
          finishedAt: null,
        },
        state: { state: "queued" },
        sources: sources.map((s) => ({ hostId: s.hostId, path: normalize(s.path) })),
        plan: null,
        index: 0,
        fileDone: 0,
        checked: false,
        targetName: null,
        target: null,
        policy: null,
        prompt: null,
        gen: 0,
        live: false,
        failedOnce: false,
        speed: 0,
        maxBytes: request.maxBytes ?? null,
      };
      jobs.push(job);
      const snapshot = structuredClone(info(job));
      emitJob(job);
      setTimeout(pump, 0);
      return snapshot;
    },

    async transferList() {
      return structuredClone(jobs.map(info));
    },

    async transferPause(jobId) {
      const job = findJob(jobId);
      const s = job.state.state;
      if (s === "queued" || s === "scanning" || s === "running") setState(job, { state: "paused" });
      pump();
    },

    async transferResume(jobId) {
      const job = findJob(jobId);
      if (job.state.state !== "paused") return;
      setState(job, { state: "queued" });
      pump();
    },

    async transferCancel(jobId) {
      cancel(findJob(jobId));
    },

    async transferRetry(jobId) {
      const job = findJob(jobId);
      if (job.state.state !== "failed" && job.state.state !== "cancelled") return;
      for (const h of hostsOf(job)) machine(h);
      stop(job);
      // Mid-file failures continue the partial file; otherwise re-check conflicts.
      job.checked = job.target !== null;
      setState(job, { state: "queued" });
      pump();
    },

    async transferClearFinished() {
      for (let i = jobs.length - 1; i >= 0; i--) {
        if (FINISHED.has(jobs[i].state.state)) jobs.splice(i, 1);
      }
    },

    async searchStart(hostId, root, query) {
      const m = machine(hostId);
      await latency(m);
      const p = normalize(root);
      const r = resolve(m, p, true);
      if (r.node.kind !== "dir") throw fsErr(m, "notDir", p);
      const searchId = id();
      searches.set(searchId, { cancelled: false });
      void runSearch(searchId, hostId, m, p, query);
      return searchId;
    },

    async searchCancel(searchId) {
      const s = searches.get(searchId);
      if (s) s.cancelled = true;
      searches.delete(searchId);
    },
  };

  return {
    methods,
    machine,
    ensureDir(hostId, path) {
      const m = machine(hostId);
      const parts = normalize(path).split("/").filter(Boolean);
      let p = "";
      for (const part of parts) {
        p = `${p}/${part}`;
        if (!tryResolve(m, p, true)) create(m, p, newDir(m, part));
      }
    },
    hostDisconnected(hostId) {
      for (const job of jobs) {
        if (FINISHED.has(job.state.state)) continue;
        if (!hostsOf(job).includes(hostId)) continue;
        fail(job, `connection to ${hostName(hostId)} lost`, true);
      }
    },
  };
}

