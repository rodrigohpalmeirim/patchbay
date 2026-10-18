// Checks GitHub releases for a newer signed build (Tauri updater) in
// production desktop builds and offers to install it.

import { isTauri } from "./api";
import { toasts } from "./stores/toasts.svelte";

const FIRST_CHECK_DELAY = 15_000;
const CHECK_INTERVAL = 6 * 60 * 60 * 1000;

let offered: string | null = null;

async function checkOnce() {
  const { check } = await import("@tauri-apps/plugin-updater");
  const update = await check();
  if (!update || offered === update.version) return;
  offered = update.version;
  toasts.show("info", `Patchbay ${update.version} is available`, "It installs in the background; Patchbay restarts when it's done.", {
    sticky: true,
    action: {
      label: "Update and restart",
      run: () => void install(update),
    },
  });
}

async function install(update: import("@tauri-apps/plugin-updater").Update) {
  const progress = toasts.show("info", `Downloading Patchbay ${update.version}…`, undefined, { sticky: true });
  try {
    await update.downloadAndInstall();
    const { relaunch } = await import("@tauri-apps/plugin-process");
    await relaunch();
  } catch (e) {
    toasts.dismiss(progress);
    toasts.show("error", "Update failed", String(e));
  }
}

/** Starts periodic update checks (desktop production builds only). */
export function startUpdateChecks() {
  if (!isTauri || import.meta.env.DEV) return;
  const run = () =>
    checkOnce().catch((e) => {
      // No releases yet, offline, or no endpoint configured: not worth a toast.
      console.warn("update check failed", e);
    });
  setTimeout(run, FIRST_CHECK_DELAY);
  setInterval(run, CHECK_INTERVAL);
}
