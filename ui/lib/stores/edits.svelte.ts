// Edit sessions (remote files opened in a local app, uploaded on save):
// seeded from editList() on startup, then kept up to date by `edit` core
// events. Conflicts arrive as `editConflict` prompts.

import { machineName } from "../files/ops";
import type { EditSession, FileEntry, HostId } from "../types";
import { sentence } from "../util/errors";
import { basename, dirname } from "../util/paths";
import { attempt } from "./backend";
import { files } from "./files.svelte";
import { toasts } from "./toasts.svelte";
import { windows } from "./windows.svelte";
import { transfers } from "./transfers.svelte";
import { ui } from "./ui.svelte";

export const editName = (s: EditSession) => basename(s.remotePath) || s.remotePath;

/** Why the last sync failed, in words for the user. */
export function failure(s: Extract<EditSession, { state: "failed" }>): string {
  if (/^not connected/i.test(s.message)) return `Not connected to ${machineName(s.hostId)}.`;
  const m = sentence(s.message);
  return /[.!?]$/.test(m) ? m : `${m}.`;
}

/** Needs the user: a failed sync or an unresolved conflict. */
export const needsAttention = (s: EditSession) => s.state === "failed" || s.state === "conflict";

class Edits {
  map = $state<Record<string, EditSession>>({});
  list = $derived(Object.values(this.map));
  attention = $derived(this.list.filter(needsAttention));

  async load() {
    const list = await attempt((b) => b.editList(), { toast: false });
    if (!list) return;
    const map: Record<string, EditSession> = {};
    for (const s of list) map[s.id] = s;
    this.map = map;
  }

  /** Applies an `edit` event (or a command result). */
  handle(s: EditSession) {
    const prev = this.map[s.id];
    if (s.state === "closed") {
      delete this.map[s.id];
      return;
    }
    this.map[s.id] = s;
    if (!prev) return;
    // Every window gets the event; the one last used tells the user.
    if (s.lastUpload !== prev.lastUpload && s.lastUpload != null) {
      files.refreshDir(s.hostId, dirname(s.remotePath) ?? "/");
      if (prev.lastUpload == null && windows.active) toasts.success(`Saved ${editName(s)} to ${machineName(s.hostId)}`, "Later saves upload automatically.");
    } else if (s.state === "failed" && prev.state !== "failed" && !s.recovered && windows.active) {
      toasts.show(
        "error",
        `Couldn't save ${editName(s)} to ${machineName(s.hostId)}`,
        `${failure(s)} Patchbay tries again when you save, and when the connection is back.`,
      );
    }
  }

  /** Downloads a remote file and opens it in the default app. */
  async open(hostId: HostId, entry: FileEntry) {
    const s = await attempt((b) => b.editOpen(hostId, entry.path), { action: `Couldn't open “${entry.name}”` });
    if (!s) return;
    const fresh = !this.map[s.id];
    this.map[s.id] = s;
    if (fresh) toasts.info(`Editing ${entry.name}`, `Saves upload to ${machineName(hostId)} automatically.`);
  }

  retry(s: EditSession) {
    return attempt((b) => b.editRetry(s.id), { action: `Couldn't retry ${editName(s)}` });
  }

  reveal(s: EditSession) {
    return attempt((b) => b.editReveal(s.id), { action: "Couldn't show the file" });
  }

  /** Reopens the local copy in the default app. */
  reopen(s: EditSession) {
    if (!s.hostId) return;
    return attempt((b) => b.editOpen(s.hostId!, s.remotePath), { action: `Couldn't open ${editName(s)}` });
  }

  /** Stops syncing; asks first if that would lose unsynced changes. */
  async stop(s: EditSession) {
    let force = false;
    if (s.dirty) {
      const { ok, checked } = await ui.confirmWithOption({
        title: `Stop editing “${editName(s)}”?`,
        message: `Your latest changes haven't been uploaded to ${machineName(s.hostId)}. Stopping deletes the local copy with them.`,
        confirmLabel: "Stop editing",
        danger: true,
        option: "Keep the local copy",
      });
      if (!ok) return;
      force = !checked;
    }
    const kept = await attempt((b) => b.editClose(s.id, force), { action: `Couldn't stop editing ${editName(s)}` });
    if (kept) toasts.info(`Kept your copy of ${editName(s)}`, kept);
  }

  /** The panel's Editing tab, opened from the sidebar indicator. */
  show() {
    transfers.showTab("editing");
  }
}

export const edits = new Edits();
