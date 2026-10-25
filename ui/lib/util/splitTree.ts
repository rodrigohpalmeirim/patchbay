// Binary split trees: terminal panes inside a terminal tab
// (terminal/layout.ts) and tab groups in the main area (stores/tabs.svelte.ts).
// Pure functions; a tree is serializable and replaced wholesale on structural
// changes.
//
// Rendering doesn't nest components per split: `computeLayout` flattens the
// tree into rectangles (fractions of the whole area), and every leaf is
// rendered in one keyed list and absolutely positioned. Restructuring the
// tree therefore never remounts a leaf's content.

export type SplitDir = "row" | "column";
export type Direction = "left" | "right" | "up" | "down";

export interface Leaf {
  /** Anything but "split". */
  type: string;
  id: string;
}

export interface Split<L extends Leaf> {
  type: "split";
  id: string;
  /** "row" = side by side (vertical divider), "column" = stacked. */
  dir: SplitDir;
  /** Fraction of the split taken by `a`. */
  ratio: number;
  a: Tree<L>;
  b: Tree<L>;
}

export type Tree<L extends Leaf> = L | Split<L>;

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface Divider {
  /** Id of the split this divider resizes. */
  splitId: string;
  dir: SplitDir;
  /** The split's own rect (for converting pointer positions to a ratio). */
  area: Rect;
  ratio: number;
}

export const RATIO_MIN = 0.1;
export const RATIO_MAX = 0.9;

let seq = 0;
/** Tells apart ids made in different windows at the same time, so a tab handed to another window stays unique there. */
const page = Math.random().toString(36).slice(2, 6);
export const newId = (prefix: string) => `${prefix}-${Date.now().toString(36)}-${page}${(seq++).toString(36)}`;

const isSplit = <L extends Leaf>(node: Tree<L>): node is Split<L> => node.type === "split";

/** The split direction that puts something on the `dir` side. */
export const dirAxis = (dir: Direction): SplitDir => (dir === "left" || dir === "right" ? "row" : "column");

export function leaves<L extends Leaf>(node: Tree<L>): L[] {
  return isSplit(node) ? [...leaves(node.a), ...leaves(node.b)] : [node];
}

export function findLeaf<L extends Leaf>(node: Tree<L>, id: string): L | null {
  if (!isSplit(node)) return node.id === id ? node : null;
  return findLeaf(node.a, id) ?? findLeaf(node.b, id);
}

export function findSplit<L extends Leaf>(node: Tree<L>, id: string): Split<L> | null {
  if (!isSplit(node)) return null;
  if (node.id === id) return node;
  return findSplit(node.a, id) ?? findSplit(node.b, id);
}

/**
 * Replaces leaf `id` with a split of it and `added` (a leaf or a whole tree),
 * which goes after/below it, or before/above with `before`.
 */
export function splitLeaf<L extends Leaf>(node: Tree<L>, id: string, dir: SplitDir, added: Tree<L>, before = false): Tree<L> {
  if (!isSplit(node)) {
    if (node.id !== id) return node;
    const [a, b] = before ? [added, node] : [node, added];
    return { type: "split", id: newId("split"), dir, ratio: 0.5, a, b };
  }
  const a = splitLeaf(node.a, id, dir, added, before);
  const b = a === node.a ? splitLeaf(node.b, id, dir, added, before) : node.b;
  return a === node.a && b === node.b ? node : { ...node, a, b };
}

/** Removes leaf `id`; its sibling takes the parent's place. Null if it was the last leaf. */
export function removeLeaf<L extends Leaf>(node: Tree<L>, id: string): Tree<L> | null {
  if (!isSplit(node)) return node.id === id ? null : node;
  const a = removeLeaf(node.a, id);
  if (a === null) return node.b;
  const b = removeLeaf(node.b, id);
  if (b === null) return node.a;
  return a === node.a && b === node.b ? node : { ...node, a, b };
}

export function clampRatio(r: number): number {
  return Math.min(RATIO_MAX, Math.max(RATIO_MIN, r));
}

/** Flattens the tree into leaf rects and dividers (fractions of the whole area). */
export function computeLayout<L extends Leaf>(root: Tree<L>): { rects: Map<string, Rect>; dividers: Divider[] } {
  const rects = new Map<string, Rect>();
  const dividers: Divider[] = [];
  const walk = (node: Tree<L>, r: Rect) => {
    if (!isSplit(node)) {
      rects.set(node.id, r);
      return;
    }
    const t = node.ratio;
    if (node.dir === "row") {
      walk(node.a, { x: r.x, y: r.y, w: r.w * t, h: r.h });
      walk(node.b, { x: r.x + r.w * t, y: r.y, w: r.w * (1 - t), h: r.h });
    } else {
      walk(node.a, { x: r.x, y: r.y, w: r.w, h: r.h * t });
      walk(node.b, { x: r.x, y: r.y + r.h * t, w: r.w, h: r.h * (1 - t) });
    }
    dividers.push({ splitId: node.id, dir: node.dir, area: r, ratio: t });
  };
  walk(root, { x: 0, y: 0, w: 1, h: 1 });
  return { rects, dividers };
}

const EPS = 1e-6;

/** The leaf next to `id` in `dir` (most overlapping, then nearest), or null. */
export function neighbor<L extends Leaf>(root: Tree<L>, id: string, dir: Direction): string | null {
  const { rects } = computeLayout(root);
  const cur = rects.get(id);
  if (!cur) return null;
  let best: { id: string; overlap: number; dist: number } | null = null;
  for (const [lid, r] of rects) {
    if (lid === id) continue;
    let adjacent: boolean;
    let overlap: number;
    let dist: number;
    if (dir === "left" || dir === "right") {
      adjacent = dir === "right" ? Math.abs(r.x - (cur.x + cur.w)) < EPS : Math.abs(r.x + r.w - cur.x) < EPS;
      overlap = Math.min(cur.y + cur.h, r.y + r.h) - Math.max(cur.y, r.y);
      dist = Math.abs(r.y + r.h / 2 - (cur.y + cur.h / 2));
    } else {
      adjacent = dir === "down" ? Math.abs(r.y - (cur.y + cur.h)) < EPS : Math.abs(r.y + r.h - cur.y) < EPS;
      overlap = Math.min(cur.x + cur.w, r.x + r.w) - Math.max(cur.x, r.x);
      dist = Math.abs(r.x + r.w / 2 - (cur.x + cur.w / 2));
    }
    if (!adjacent || overlap <= EPS) continue;
    if (!best || overlap > best.overlap + EPS || (Math.abs(overlap - best.overlap) < EPS && dist < best.dist)) {
      best = { id: lid, overlap, dist };
    }
  }
  return best?.id ?? null;
}

/** A neighbor in any direction (left, up, right, down first), or null if `id` is alone. */
export function anyNeighbor<L extends Leaf>(root: Tree<L>, id: string): string | null {
  for (const dir of ["left", "up", "right", "down"] as const) {
    const n = neighbor(root, id, dir);
    if (n) return n;
  }
  return null;
}
