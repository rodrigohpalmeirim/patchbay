// Pane tree of a terminal tab: a binary tree of splits whose leaves are
// terminal panes. Pure functions; the tree lives (serializable) in the tab
// descriptor and is replaced wholesale on structural changes.
//
// Rendering doesn't nest components per split: `computeLayout` flattens the
// tree into rectangles (fractions of the tab area), and every pane is
// rendered in one keyed list and absolutely positioned. Restructuring the
// tree therefore never remounts a pane, so its xterm instance and shell
// survive splits and closes of neighbours.

export type SplitDir = "row" | "column";
export type Direction = "left" | "right" | "up" | "down";

export interface PaneLeaf {
  type: "pane";
  id: string;
  /** Start directory; updated from OSC 7 so restarts/splits reuse it. */
  cwd: string | null;
}

export interface PaneSplit {
  type: "split";
  id: string;
  /** "row" = side by side (vertical divider), "column" = stacked. */
  dir: SplitDir;
  /** Fraction of the split taken by `a`. */
  ratio: number;
  a: PaneNode;
  b: PaneNode;
}

export type PaneNode = PaneLeaf | PaneSplit;

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
export const newId = (prefix: string) => `${prefix}-${Date.now().toString(36)}-${(seq++).toString(36)}`;

export function newPane(cwd: string | null = null): PaneLeaf {
  return { type: "pane", id: newId("pane"), cwd };
}

export function leaves(node: PaneNode): PaneLeaf[] {
  return node.type === "pane" ? [node] : [...leaves(node.a), ...leaves(node.b)];
}

export function findLeaf(node: PaneNode, id: string): PaneLeaf | null {
  if (node.type === "pane") return node.id === id ? node : null;
  return findLeaf(node.a, id) ?? findLeaf(node.b, id);
}

export function findSplit(node: PaneNode, id: string): PaneSplit | null {
  if (node.type === "pane") return null;
  if (node.id === id) return node;
  return findSplit(node.a, id) ?? findSplit(node.b, id);
}

/** Replaces leaf `id` with a split of it and `added` (added goes after/below). */
export function splitPane(node: PaneNode, id: string, dir: SplitDir, added: PaneLeaf): PaneNode {
  if (node.type === "pane") {
    return node.id === id ? { type: "split", id: newId("split"), dir, ratio: 0.5, a: node, b: added } : node;
  }
  const a = splitPane(node.a, id, dir, added);
  const b = a === node.a ? splitPane(node.b, id, dir, added) : node.b;
  return a === node.a && b === node.b ? node : { ...node, a, b };
}

/** Removes leaf `id`; its sibling takes the parent's place. Null if it was the last pane. */
export function removePane(node: PaneNode, id: string): PaneNode | null {
  if (node.type === "pane") return node.id === id ? null : node;
  const a = removePane(node.a, id);
  if (a === null) return node.b;
  const b = removePane(node.b, id);
  if (b === null) return node.a;
  return a === node.a && b === node.b ? node : { ...node, a, b };
}

export function clampRatio(r: number): number {
  return Math.min(RATIO_MAX, Math.max(RATIO_MIN, r));
}

/** Flattens the tree into pane rects and dividers (fractions of the whole area). */
export function computeLayout(root: PaneNode): { panes: Map<string, Rect>; dividers: Divider[] } {
  const panes = new Map<string, Rect>();
  const dividers: Divider[] = [];
  const walk = (node: PaneNode, r: Rect) => {
    if (node.type === "pane") {
      panes.set(node.id, r);
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
  return { panes, dividers };
}

const EPS = 1e-6;

/** The pane next to `id` in `dir` (most overlapping, then nearest), or null. */
export function neighbor(root: PaneNode, id: string, dir: Direction): string | null {
  const { panes } = computeLayout(root);
  const cur = panes.get(id);
  if (!cur) return null;
  let best: { id: string; overlap: number; dist: number } | null = null;
  for (const [pid, r] of panes) {
    if (pid === id) continue;
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
      best = { id: pid, overlap, dist };
    }
  }
  return best?.id ?? null;
}
