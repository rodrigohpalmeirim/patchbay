// Wires backend events into the stores. Called once per window from
// App.svelte; every window runs this with its own stores.

import { getBackend, isTauri } from "../api";
import type { HostId } from "../types";
import { machineName } from "../files/ops";
import { plural, setHomeDir } from "../util/format";
import { appSettings } from "./appSettings.svelte";
import { attempt } from "./backend";
import { connections } from "./connections.svelte";
import { edits } from "./edits.svelte";
import { dnd, installOsDrop } from "./dnd.svelte";
import { CLIPBOARD_TOPIC, filePrefs, files, PREFS_TOPIC, searches } from "./files.svelte";
import { forwards } from "./forwards.svelte";
import { hosts } from "./hosts.svelte";
import { prompts } from "./prompts.svelte";
import { session } from "./session.svelte";
import { settings, SETTINGS_TOPIC } from "./settings.svelte";
import { snippets } from "./snippets.svelte";
import { tabs } from "./tabs.svelte";
import { terminals } from "./terminals.svelte";
import { transfers } from "./transfers.svelte";
import { ui } from "./ui.svelte";
import { windows } from "./windows.svelte";

export async function startApp(): Promise<() => void> {
  try {
    return await start();
  } finally {
    // Never leave the main area blank if startup failed half-way.
    session.pending = false;
  }
}

async function start(): Promise<() => void> {
  const backend = await getBackend();
  // The window's sidebar, right away.
  const loading = session.load();
  const unlisten = await Promise.all([
    backend.onCoreEvent((ev) => {
      switch (ev.type) {
        case "hostsChanged":
          void hosts.refresh().then(() => {
            tabs.retarget((id) => hosts.currentId(id));
            tabs.prune((id) => hosts.byId.has(id));
            for (const id of Object.keys(connections.map)) {
              if (!hosts.byId.has(id)) connections.forget(id);
            }
          });
          break;
        case "authBanner":
          connections.banner(ev.hostId, ev.message);
          break;
        case "connection": {
          const { type: _type, hostId, ...state } = ev;
          connections.handle(hostId, state);
          if (state.state === "disconnected" || state.state === "reconnecting") files.onDisconnected(hostId);
          if (state.state === "disconnected") terminals.hostDisconnected(hostId, state.reason);
          break;
        }
        case "transfer": {
          const { type: _type, ...info } = ev;
          transfers.handle(info);
          break;
        }
        case "search":
          searches.handle(ev);
          break;
        case "terminalExit":
          terminals.handleExit(ev);
          break;
        case "snippetsChanged":
          if (snippets.loaded) void snippets.load();
          break;
        case "portForward": {
          const { type: _type, ...status } = ev;
          forwards.handle(status);
          break;
        }
        case "portForwardsChanged":
          void forwards.load();
          break;
        case "settingsChanged": {
          const { type: _type, ...s } = ev;
          appSettings.handle(s);
          break;
        }
        case "edit": {
          const { type: _type, ...session } = ev;
          edits.handle(session);
          break;
        }
      }
    }),
    backend.onPrompt((p) => prompts.push(p)),
    backend.onPromptClosed((id) => prompts.close(id)),
    backend.onShared((topic, value) => adoptShared(topic, value)),
    backend.onCloseRequested(() => void confirmClose()),
    backend.onTabDragOver((drag) => dnd.tabOver(drag)),
    windows.start(),
    installOsDrop(),
  ]);
  // Prompts sent here before this page listened (a new or reloaded window).
  for (const p of (await attempt((b) => b.windowPrompts(), { toast: false })) ?? []) prompts.push(p);
  if (isTauri) {
    const { homeDir } = await import("@tauri-apps/api/path");
    setHomeDir(await homeDir().catch(() => null));
  }
  await Promise.all([
    hosts.refresh(),
    transfers.load(),
    edits.load(),
    forwards.load(),
    appSettings.load(),
    attempt((b) => b.shared(CLIPBOARD_TOPIC), { toast: false }).then((clip) => files.adoptClipboard(clip)),
  ]);
  const current = await backend.listConnections();
  for (const [id, state] of Object.entries(current)) connections.handle(id, state);
  await loading;
  await session.restore();
  const stopSession = session.start();
  // Tabs moved here, e.g. into this new window.
  windows.ready();
  return () => {
    stopSession();
    unlisten.forEach((u) => u());
  };
}

/** Window-local state changed in another window. */
function adoptShared(topic: string, value: unknown) {
  if (topic === SETTINGS_TOPIC) settings.adopt(value);
  else if (topic === PREFS_TOPIC) filePrefs.adopt(value);
  else if (topic === CLIPBOARD_TOPIC) files.adoptClipboard(value);
}

/**
 * The user closed this window while other windows stay open, and shells run
 * in it: they end with it, so ask first.
 */
async function confirmClose() {
  const running = [...terminals.sessions.values()].filter((s) => s.status === "running");
  const where = (hostId: HostId | null) => (hostId ? `on ${machineName(hostId)}` : "on this computer");
  if (running.length) {
    const ok = await ui.confirm({
      title: "Close this window?",
      message:
        running.length === 1
          ? `Its terminal ${where(running[0].hostId)} is still running and will be closed.`
          : `${plural(running.length, "terminal")} are still running in it and will be closed.`,
      confirmLabel: "Close window",
      danger: true,
      details: running.length > 1 ? running.map((s) => `${s.title ?? s.shell ?? "Shell"} ${where(s.hostId)}`) : undefined,
    });
    if (!ok) return;
  }
  await attempt((b) => b.windowClose(), { action: "Couldn't close the window" });
}
