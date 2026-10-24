// Pane tree of a terminal tab: a split tree (util/splitTree.ts) whose leaves
// are terminal panes. The tree lives (serializable) in the tab descriptor.

import { newId, removeLeaf, splitLeaf, type Split, type SplitDir, type Tree } from "../util/splitTree";

export {
  anyNeighbor,
  clampRatio,
  computeLayout,
  findLeaf,
  findSplit,
  leaves,
  neighbor,
  newId,
  RATIO_MAX,
  RATIO_MIN,
  type Direction,
  type Divider,
  type Rect,
  type SplitDir,
} from "../util/splitTree";

export interface PaneLeaf {
  type: "pane";
  id: string;
  /** Start directory; updated from OSC 7 so restarts/splits reuse it. */
  cwd: string | null;
}

export type PaneSplit = Split<PaneLeaf>;
export type PaneNode = Tree<PaneLeaf>;

export function newPane(cwd: string | null = null): PaneLeaf {
  return { type: "pane", id: newId("pane"), cwd };
}

/** Replaces leaf `id` with a split of it and `added` (added goes after/below). */
export function splitPane(node: PaneNode, id: string, dir: SplitDir, added: PaneLeaf): PaneNode {
  return splitLeaf(node, id, dir, added);
}

/** Removes leaf `id`; its sibling takes the parent's place. Null if it was the last pane. */
export function removePane(node: PaneNode, id: string): PaneNode | null {
  return removeLeaf(node, id);
}
