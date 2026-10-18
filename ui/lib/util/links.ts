import { isTauri } from "../api";
import { toasts } from "../stores/toasts.svelte";
import { toCommandError } from "./errors";

/** Opens `url` in the default browser. */
export async function openInBrowser(url: string) {
  try {
    if (isTauri) {
      const { openUrl } = await import("@tauri-apps/plugin-opener");
      await openUrl(url);
    } else {
      window.open(url, "_blank", "noopener,noreferrer");
    }
  } catch (e) {
    toasts.error(toCommandError(e), "Couldn't open the browser");
  }
}
