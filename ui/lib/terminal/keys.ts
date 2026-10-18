// Keyboard shortcuts for terminals.
//
// Shells need most Ctrl and Alt combinations (Ctrl+W/R/U/N/F/\, Alt+B/F, …),
// so terminal actions use Ctrl+Shift on Windows/Linux and Cmd on macOS, where
// Cmd never reaches the shell anyway. While a terminal has focus only a few
// app shortcuts stay active (see `passesToApp`); everything else goes to the
// shell.
//
// Terminal shortcut map:            Windows / Linux        macOS
//   Copy / Paste                    Ctrl+Shift+C / V       ⌘C / ⌘V   (also Ctrl+Insert / Shift+Insert)
//   Find                            Ctrl+Shift+F           ⌘F
//   Snippets                        Ctrl+Shift+S           ⌘⇧S
//   Split right / down              Ctrl+Shift+D / E       ⌘D / ⌘⇧D
//   Close pane                      Ctrl+Shift+W           ⌘W
//   Focus pane left/right/up/down   Alt+Arrows             ⌥⌘Arrows
//   Zoom pane                       Ctrl+Shift+Enter       ⌘⇧Enter
//   Clear                           Ctrl+Shift+K           ⌘K
//   Select all                      Ctrl+Shift+A           ⌘A
//   New terminal (app-wide)         Ctrl+Shift+T           ⌘T
// App shortcuts that keep working inside a terminal:
//   Ctrl+Tab / Ctrl+Shift+Tab, Ctrl+PageUp/PageDown, Mod+1…9, Mod+, (settings),
//   Mod+Shift+J (transfers), Mod+Shift+P (port forwards), F6 (other split
//   side), Mod+Shift+T; on macOS all
//   other ⌘ shortcuts too (⌘N, ⌘\ …).

import type { Direction } from "./layout";
import { isMac, isWindows } from "../util/platform";

export type TermAction =
  | "copy"
  | "paste"
  | "find"
  | "snippets"
  | "splitRight"
  | "splitDown"
  | "closePane"
  | "zoom"
  | "clear"
  | "selectAll"
  | `focus-${Direction}`;

const ARROWS: Record<string, Direction> = { ArrowLeft: "left", ArrowRight: "right", ArrowUp: "up", ArrowDown: "down" };

/** The terminal action for a key event, if it is one (callers act on keydown only). */
export function terminalAction(e: KeyboardEvent): TermAction | null {
  const k = e.key.length === 1 ? e.key.toLowerCase() : e.key;
  if (isMac) {
    if (e.metaKey && e.altKey && !e.ctrlKey && ARROWS[k]) return `focus-${ARROWS[k]}`;
    if (!e.metaKey || e.ctrlKey || e.altKey) return null;
    if (e.shiftKey) {
      if (k === "d") return "splitDown";
      if (k === "s") return "snippets";
      if (k === "Enter") return "zoom";
      return null;
    }
    switch (k) {
      case "c":
        return "copy";
      case "v":
        return "paste";
      case "f":
        return "find";
      case "d":
        return "splitRight";
      case "w":
        return "closePane";
      case "k":
        return "clear";
      case "a":
        return "selectAll";
    }
    return null;
  }
  if (e.altKey && !e.ctrlKey && !e.shiftKey && !e.metaKey && ARROWS[k]) return `focus-${ARROWS[k]}`;
  if (e.ctrlKey && !e.shiftKey && !e.altKey && k === "Insert") return "copy";
  if (e.shiftKey && !e.ctrlKey && !e.altKey && k === "Insert") return "paste";
  // Windows Terminal convention; Ctrl+V's readline meaning (quoted insert) is rarely needed.
  if (isWindows && e.ctrlKey && !e.shiftKey && !e.altKey && !e.metaKey && k === "v") return "paste";
  if (!e.ctrlKey || !e.shiftKey || e.altKey || e.metaKey) return null;
  switch (k) {
    case "c":
      return "copy";
    case "v":
      return "paste";
    case "f":
      return "find";
    case "s":
      return "snippets";
    case "d":
      return "splitRight";
    case "e":
      return "splitDown";
    case "w":
      return "closePane";
    case "Enter":
      return "zoom";
    case "k":
      return "clear";
    case "a":
      return "selectAll";
  }
  return null;
}

/**
 * App-level shortcuts that should reach App.svelte even while a terminal has
 * focus (xterm must not consume them).
 */
export function passesToApp(e: KeyboardEvent): boolean {
  if (e.key === "F6") return true;
  if (e.ctrlKey && (e.key === "Tab" || e.key === "PageUp" || e.key === "PageDown")) return true;
  if (isMac) return e.metaKey;
  if (!e.ctrlKey || e.altKey || e.metaKey) return false;
  if (/^[1-9]$/.test(e.key) && !e.shiftKey) return true;
  if (e.key === "," && !e.shiftKey) return true;
  return e.shiftKey && (e.code === "KeyJ" || e.code === "KeyT" || e.code === "KeyP");
}

/** Is the event target inside a terminal pane (xterm's input textarea etc.)? */
export function inTerminal(target: EventTarget | null): boolean {
  return target instanceof Element && !!target.closest("[data-terminal-pane]");
}

const LABELS: Record<TermAction, [string, string]> = {
  copy: ["Ctrl+Shift+C", "⌘C"],
  paste: [isWindows ? "Ctrl+V" : "Ctrl+Shift+V", "⌘V"],
  find: ["Ctrl+Shift+F", "⌘F"],
  snippets: ["Ctrl+Shift+S", "⌘⇧S"],
  splitRight: ["Ctrl+Shift+D", "⌘D"],
  splitDown: ["Ctrl+Shift+E", "⌘⇧D"],
  closePane: ["Ctrl+Shift+W", "⌘W"],
  zoom: ["Ctrl+Shift+Enter", "⌘⇧↩"],
  clear: ["Ctrl+Shift+K", "⌘K"],
  selectAll: ["Ctrl+Shift+A", "⌘A"],
  "focus-left": ["Alt+←", "⌥⌘←"],
  "focus-right": ["Alt+→", "⌥⌘→"],
  "focus-up": ["Alt+↑", "⌥⌘↑"],
  "focus-down": ["Alt+↓", "⌥⌘↓"],
};

/** Human-readable shortcut for menus and tooltips. */
export function keyLabel(action: TermAction): string {
  return LABELS[action][isMac ? 1 : 0];
}

/** "New terminal" shortcut (handled by App). */
export const NEW_TERMINAL_KEY = isMac ? "⌘T" : "Ctrl+Shift+T";

export function isNewTerminalKey(e: KeyboardEvent): boolean {
  if (e.altKey) return false;
  return isMac ? e.metaKey && !e.shiftKey && !e.ctrlKey && e.key.toLowerCase() === "t" : e.ctrlKey && e.shiftKey && e.code === "KeyT";
}
