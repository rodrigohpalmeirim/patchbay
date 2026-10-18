export const isMac =
  typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);

export const isWindows =
  typeof navigator !== "undefined" && /Win/.test(navigator.platform || navigator.userAgent);

/** Label for the primary modifier key. */
export const modKey = isMac ? "⌘" : "Ctrl";

/** True for Cmd on macOS, Ctrl elsewhere. */
export function hasMod(e: KeyboardEvent | MouseEvent): boolean {
  return isMac ? e.metaKey : e.ctrlKey;
}

/** True if the event target is a text field where typing shouldn't trigger shortcuts. */
export function isEditable(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  const tag = target.tagName;
  if (tag === "TEXTAREA" || tag === "SELECT") return true;
  if (tag === "INPUT") {
    const type = (target as HTMLInputElement).type;
    return !["checkbox", "radio", "button", "submit", "reset", "range", "color"].includes(type);
  }
  return false;
}
