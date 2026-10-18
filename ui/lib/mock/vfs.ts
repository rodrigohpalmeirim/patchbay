// In-memory filesystem model for the mock backend: nodes, POSIX path helpers,
// path resolution (with symlinks), FileEntry conversion and a small builder
// DSL used by trees.ts.

import type { CommandError, FileEntry } from "../types";
import { byteLength, defaultContent, hash, rng, type ContentSpec } from "./content";

interface NodeBase {
  name: string;
  /** Unix seconds. */
  modified: number;
  permissions: number;
  owner: string | null;
  group: string | null;
  /** Hidden even without a leading dot (Windows attributes). */
  hidden?: boolean;
}

export interface FileNode extends NodeBase {
  kind: "file";
  size: number;
  content: ContentSpec | null;
}

export interface DirNode extends NodeBase {
  kind: "dir";
  /** Null until a lazy directory is first touched. */
  children: Map<string, Node> | null;
  lazy: (() => Node[]) | null;
  /** Listing / entering fails with permission denied. */
  denied: boolean;
  /** Extra latency when listed (huge directories). */
  slow: boolean;
}

export interface LinkNode extends NodeBase {
  kind: "symlink";
  target: string;
}

export type Node = FileNode | DirNode | LinkNode;

export interface Machine {
  key: string;
  label: string;
  root: DirNode;
  home: string;
  user: string;
  group: string | null;
  remote: boolean;
  /** Remote shell available (chown by name, fast search). */
  exec: boolean;
  /** Same LAN as this computer (faster simulated transfers). */
  lan: boolean;
  windows: boolean;
  /** If set, writes are only allowed under these paths (chrooted SFTP share). */
  writableRoots?: string[];
}

export function kids(d: DirNode): Map<string, Node> {
  if (!d.children) {
    d.children = new Map((d.lazy?.() ?? []).map((n) => [n.name, n]));
    d.lazy = null;
  }
  return d.children;
}

// ------------------------------------------------------------------ paths

export function splitPath(path: string): string[] {
  const out: string[] = [];
  for (const p of path.replace(/\\/g, "/").split("/")) {
    if (!p || p === ".") continue;
    if (p === "..") out.pop();
    else out.push(p);
  }
  return out;
}

export const normalize = (path: string): string => "/" + splitPath(path).join("/");
export const join = (dir: string, name: string): string => (dir === "/" ? `/${name}` : `${dir}/${name}`);
export function dirname(path: string): string {
  const parts = splitPath(path);
  parts.pop();
  return "/" + parts.join("/");
}
export const basename = (path: string): string => splitPath(path).pop() ?? "";

// ----------------------------------------------------------------- errors

export type FsErrorKind = "notFound" | "denied" | "notDir" | "isDir" | "exists" | "loop" | "invalid";

const MESSAGES: Record<FsErrorKind, string> = {
  notFound: "no such file or directory",
  denied: "permission denied",
  notDir: "not a directory",
  isDir: "is a directory",
  exists: "file exists",
  loop: "too many levels of symbolic links",
  invalid: "invalid argument",
};

/** Error kinds the real backend reports for each failure (see types.ts ErrorKind). */
const KINDS: Partial<Record<FsErrorKind, CommandError["kind"]>> = {
  notFound: "notFound",
  denied: "permissionDenied",
  exists: "alreadyExists",
};

export function fsErr(m: Machine, kind: FsErrorKind, path: string, detail?: string): CommandError {
  return { kind: KINDS[kind] ?? (m.remote ? "sftp" : "io"), message: `${detail ?? MESSAGES[kind]}: ${path}` };
}

// ------------------------------------------------------------- resolution

export interface Resolved {
  node: Node;
  /** Canonical path of `node` (symlinks resolved). */
  real: string;
  parent: DirNode | null;
}

function linkTargetPath(linkPath: string, target: string): string {
  return target.startsWith("/") ? normalize(target) : normalize(join(dirname(linkPath), target));
}

/** Resolves `path`; intermediate symlinks are always followed, the last one only if `follow`. */
export function resolve(m: Machine, path: string, follow: boolean, depth = 0): Resolved {
  if (depth > 16) throw fsErr(m, "loop", path);
  const parts = splitPath(path);
  let node: Node = m.root;
  let real = "/";
  let parent: DirNode | null = null;
  for (const part of parts) {
    if (node.kind === "symlink") {
      const r = resolve(m, linkTargetPath(real, node.target), true, depth + 1);
      node = r.node;
      real = r.real;
    }
    if (node.kind !== "dir") throw fsErr(m, "notDir", path);
    if (node.denied) throw fsErr(m, "denied", path);
    const next = kids(node).get(part);
    if (!next) throw fsErr(m, "notFound", path);
    parent = node;
    node = next;
    real = join(real, part);
  }
  if (follow && node.kind === "symlink") {
    return resolve(m, linkTargetPath(real, node.target), true, depth + 1);
  }
  return { node, real, parent };
}

export function tryResolve(m: Machine, path: string, follow: boolean): Resolved | null {
  try {
    return resolve(m, path, follow);
  } catch {
    return null;
  }
}

export function toEntry(m: Machine, path: string, node: Node): FileEntry {
  let size = node.kind === "file" ? node.size : node.kind === "dir" ? (m.windows ? 0 : 4096) : byteLength(node.target);
  let targetKind: FileEntry["targetKind"] = null;
  if (node.kind === "symlink") {
    const t = tryResolve(m, path, true);
    if (t) {
      targetKind = t.node.kind;
      if (t.node.kind === "file") size = t.node.size;
    }
  }
  return {
    name: node.name,
    path,
    kind: node.kind,
    linkTarget: node.kind === "symlink" ? node.target : null,
    targetKind,
    size,
    modified: Math.floor(node.modified),
    permissions: node.permissions,
    owner: node.owner,
    group: node.group,
    hidden: node.name.startsWith(".") || !!node.hidden,
  };
}

/** Is `path` inside the machine's writable area (if it has one)? */
export function inWritableRoot(m: Machine, path: string): boolean {
  if (!m.writableRoots) return true;
  const p = normalize(path);
  return m.writableRoots.some((r) => p === r || p.startsWith(r + "/"));
}

/** May the machine's user create/remove entries in `dir` (at `path`)? */
export function canWrite(m: Machine, dir: DirNode, path: string): boolean {
  if (dir.denied || !inWritableRoot(m, path)) return false;
  if (m.user === "root") return true;
  if (dir.owner === null) return (dir.permissions & 0o200) !== 0;
  if (dir.owner === m.user) return (dir.permissions & 0o200) !== 0;
  if (dir.group !== null && dir.group === m.group) return (dir.permissions & 0o020) !== 0;
  return (dir.permissions & 0o002) !== 0;
}

export function canChmod(m: Machine, node: Node, path: string): boolean {
  if (!inWritableRoot(m, path)) return false;
  return m.user === "root" || node.owner === null || node.owner === m.user;
}

/** Deep copy (lazy dirs are materialized). */
export function cloneNode(n: Node, name = n.name): Node {
  if (n.kind === "dir") {
    const children = new Map<string, Node>();
    for (const c of kids(n).values()) children.set(c.name, cloneNode(c));
    return { ...n, name, children, lazy: null };
  }
  return { ...n, name };
}

// ---------------------------------------------------------------- builder

export const KB = 1024;
export const MB = 1024 * KB;
export const GB = 1024 * MB;

export interface NodeOpts {
  perm?: number;
  owner?: string | null;
  group?: string | null;
  /** Days ago (fractional ok). Random within ~2 years if omitted. */
  age?: number;
  hidden?: boolean;
  denied?: boolean;
  slow?: boolean;
}

/** Tree-building helpers with per-machine defaults and a seeded clock. */
export class Builder {
  readonly r: () => number;
  readonly now = Math.floor(Date.now() / 1000);

  constructor(
    seed: string,
    public owner: string | null,
    public group: string | null,
  ) {
    this.r = rng(hash(seed));
  }

  /** Unix time `days` ago with a little jitter; random recent-skewed time if undefined. */
  time(days?: number): number {
    const d = days ?? Math.pow(this.r(), 2.2) * 720;
    return this.now - Math.floor(d * 86400 + this.r() * 3600);
  }

  #base(name: string, o: NodeOpts, perm: number) {
    return {
      name,
      modified: this.time(o.age),
      permissions: o.perm ?? perm,
      owner: o.owner !== undefined ? o.owner : this.owner,
      group: o.group !== undefined ? o.group : this.group,
      hidden: o.hidden,
    };
  }

  dir(name: string, children: Node[] = [], o: NodeOpts = {}): DirNode {
    return {
      ...this.#base(name, o, 0o755),
      kind: "dir",
      children: new Map(children.map((c) => [c.name, c])),
      lazy: null,
      denied: o.denied ?? false,
      slow: o.slow ?? false,
    };
  }

  lazyDir(name: string, gen: () => Node[], o: NodeOpts = {}): DirNode {
    return { ...this.#base(name, o, 0o755), kind: "dir", children: null, lazy: gen, denied: o.denied ?? false, slow: o.slow ?? false };
  }

  /** File with content inferred from the name; `size` for binary/log files. */
  file(name: string, size?: number, o: NodeOpts = {}): FileNode {
    const content = defaultContent(name, hash(name) ^ Math.floor(this.r() * 1e9));
    let s = size;
    if (s === undefined) {
      if (content.t === "text") s = byteLength(content.text);
      else s = Math.floor(2 * KB + this.r() * 400 * KB);
    }
    return { ...this.#base(name, o, 0o644), kind: "file", size: Math.floor(s), content };
  }

  text(name: string, text: string, o: NodeOpts = {}): FileNode {
    return { ...this.#base(name, o, 0o644), kind: "file", size: byteLength(text), content: { t: "text", text } };
  }

  with(name: string, size: number, content: ContentSpec, o: NodeOpts = {}): FileNode {
    return { ...this.#base(name, o, 0o644), kind: "file", size: Math.floor(size), content };
  }

  link(name: string, target: string, o: NodeOpts = {}): LinkNode {
    return { ...this.#base(name, o, 0o777), kind: "symlink", target };
  }

  /** Random size between `lo` and `hi` bytes (log-uniform). */
  size(lo: number, hi: number): number {
    return Math.floor(Math.exp(Math.log(lo) + this.r() * (Math.log(hi) - Math.log(lo))));
  }

  pick<T>(xs: readonly T[]): T {
    return xs[Math.floor(this.r() * xs.length)];
  }
}
