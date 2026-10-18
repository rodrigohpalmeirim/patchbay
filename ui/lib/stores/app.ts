// Wires backend events into the stores. Called once from App.svelte.

import { getBackend, isTauri } from "../api";
import { setHomeDir } from "../util/format";
import { appSettings } from "./appSettings.svelte";
import { connections } from "./connections.svelte";
import { edits } from "./edits.svelte";
import { installOsDrop } from "./dnd.svelte";
import { files, searches } from "./files.svelte";
import { forwards } from "./forwards.svelte";
import { hosts } from "./hosts.svelte";
import { prompts } from "./prompts.svelte";
import { session } from "./session.svelte";
import { snippets } from "./snippets.svelte";
import { tabs } from "./tabs.svelte";
import { terminals } from "./terminals.svelte";
import { transfers } from "./transfers.svelte";

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
  const unlisten = await Promise.all([
    backend.onCoreEvent((ev) => {
      switch (ev.type) {
        case "hostsChanged":
          void hosts.refresh().then(() => {
            tabs.prune((id) => hosts.byId.has(id));
            for (const id of Object.keys(connections.map)) {
              if (!hosts.byId.has(id)) connections.forget(id);
            }
          });
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
    installOsDrop(),
  ]);
  if (isTauri) {
    const { homeDir } = await import("@tauri-apps/api/path");
    setHomeDir(await homeDir().catch(() => null));
  }
  await Promise.all([hosts.refresh(), transfers.load(), edits.load(), forwards.load(), appSettings.load()]);
  const current = await backend.listConnections();
  for (const [id, state] of Object.entries(current)) connections.handle(id, state);
  session.restore();
  const stopSession = session.start();
  return () => {
    stopSession();
    unlisten.forEach((u) => u());
  };
}
