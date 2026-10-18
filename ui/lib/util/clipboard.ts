// Clipboard access that works in the desktop app (native clipboard via the
// Tauri plugin; webview clipboard APIs may prompt or be blocked) and in a
// plain browser during development.

import { isTauri } from "../api";

export async function writeClipboard(text: string): Promise<void> {
  if (isTauri) {
    const { writeText } = await import("@tauri-apps/plugin-clipboard-manager");
    return writeText(text);
  }
  return navigator.clipboard.writeText(text);
}

export async function readClipboard(): Promise<string> {
  if (isTauri) {
    const { readText } = await import("@tauri-apps/plugin-clipboard-manager");
    return readText();
  }
  return navigator.clipboard.readText();
}
