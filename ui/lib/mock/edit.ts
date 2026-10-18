// Mock implementations of the edit*, openLocal and dragout* backend commands.
//
// Editing: opening downloads instantly; ~2.5 s later the "editor" saves and
// the save is uploaded (or fails while the host is disconnected and is
// retried when it connects again). Files ending in .conf get a second save
// ~5 s later that runs into a remote change -> `editConflict` prompt.
//
// Drag-out: copies into ~/.cache/patchbay/dragout/<id> with a background
// transfer; the native drag itself just reports "dropped".

import type { Backend } from "../api";
import type { CommandError, CoreEvent, EditSession, EditState, HostId, Prompt, PromptResponse } from "../types";
import type { createFiles } from "./files";
import { basename } from "./vfs";

type Files = ReturnType<typeof createFiles>;

export interface EditDeps {
  isConnected(id: HostId): boolean;
  emit(e: CoreEvent): void;
  ask(hostId: HostId | null, prompt: Prompt, signal: AbortSignal): Promise<PromptResponse>;
  files: Files;
}

type EditMethods = Pick<
  Backend,
  "editOpen" | "editList" | "editClose" | "editRetry" | "editReveal" | "openLocal" | "dragoutPrepare" | "dragoutRelease" | "startNativeDrag"
>;

const MAX_EDIT = 100 * 1024 * 1024;
const MAX_DRAGOUT = 200 * 1024 * 1024;
const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));
const uid = () => (typeof crypto !== "undefined" && "randomUUID" in crypto ? crypto.randomUUID() : Math.random().toString(16).slice(2));
const err = (kind: CommandError["kind"], message: string): CommandError => ({ kind, message });

interface Session {
  info: EditSession;
  /** Pending conflict prompt. */
  prompt: AbortController | null;
  /** A save is waiting for the host to reconnect. */
  pendingUpload: boolean;
  /** The next upload runs into a remote change. */
  remoteChanged: boolean;
}

export function createEdits(deps: EditDeps): { methods: EditMethods; hostConnected(id: HostId): void } {
  const sessions = new Map<string, Session>();
  const dragouts = new Map<string, { transfer: string; dir: string }>();

  const emit = (s: Session) => deps.emit({ type: "edit", ...structuredClone(s.info) });
  const set = (s: Session, state: EditState, patch: Partial<EditSession> = {}) => {
    s.info = { ...s.info, ...patch, ...state } as EditSession;
    emit(s);
  };
  const alive = (s: Session) => sessions.get(s.info.id) === s && s.info.state !== "stopped";

  async function upload(s: Session): Promise<void> {
    if (!alive(s)) return;
    set(s, { state: "uploading" }, { dirty: true });
    await sleep(600);
    if (!alive(s)) return;
    if (s.info.hostId && !deps.isConnected(s.info.hostId)) {
      s.pendingUpload = true;
      set(s, { state: "failed", message: "not connected" });
      return;
    }
    if (s.remoteChanged) return conflict(s);
    const now = Date.now();
    s.pendingUpload = false;
    set(s, { state: "synced", at: now }, { dirty: false, recovered: false, lastUpload: now });
  }

  async function conflict(s: Session): Promise<void> {
    set(s, { state: "conflict" });
    s.prompt?.abort();
    const abort = new AbortController();
    s.prompt = abort;
    const size = 2048 + Math.round(Math.random() * 400);
    let response: PromptResponse;
    try {
      response = await deps.ask(
        s.info.hostId,
        {
          kind: "editConflict",
          sessionId: s.info.id,
          remotePath: s.info.remotePath,
          remote: { size: size + 37, modified: Math.floor(Date.now() / 1000) - 40 },
          local: { size, modified: Math.floor(Date.now() / 1000) },
        },
        abort.signal,
      );
    } catch {
      return;
    }
    if (s.prompt === abort) s.prompt = null;
    if (response.kind !== "editConflict" || !sessions.has(s.info.id)) return;
    switch (response.resolution) {
      case "overwrite":
        s.remoteChanged = false;
        return upload(s);
      case "discardLocal": {
        s.remoteChanged = false;
        set(s, { state: "syncing" });
        await sleep(500);
        set(s, { state: "synced", at: Date.now() }, { dirty: false });
        return;
      }
      case "keepLocal":
        set(s, { state: "stopped" }, { dirty: true });
    }
  }

  /** The scripted "editor": a save, and for .conf files a conflicting one. */
  async function simulateEditor(s: Session) {
    await sleep(2500);
    await upload(s);
    if (!s.info.remotePath.endsWith(".conf")) return;
    await sleep(5000);
    if (!alive(s)) return;
    s.remoteChanged = true;
    await upload(s);
  }

  const get = (id: string) => {
    const s = sessions.get(id);
    if (!s) throw err("notFound", `not found: edit session ${id}`);
    return s;
  };

  const methods: EditMethods = {
    async editOpen(hostId, path) {
      const existing = [...sessions.values()].find((s) => s.info.hostId === hostId && s.info.remotePath === path);
      if (existing) return structuredClone(existing.info);
      const entry = await deps.files.methods.fsStat(hostId, path);
      if (entry.kind === "dir") throw err("unsupported", `not supported: ${path} is not a regular file; only files can be edited`);
      if (entry.size > MAX_EDIT) {
        throw err("unsupported", `not supported: ${entry.name} is too large to edit (${Math.ceil(entry.size / 1048576)} MiB; the limit is 100 MiB)`);
      }
      await sleep(Math.min(1200, 150 + entry.size / 20000));
      const id = uid();
      const home = await deps.files.methods.fsHome(null);
      const s: Session = {
        info: {
          id,
          hostId,
          remotePath: path,
          localPath: `${home}/.cache/patchbay/edit/${id}/${basename(path)}`,
          dirty: false,
          recovered: false,
          lastUpload: null,
          state: "synced",
          at: Date.now(),
        },
        prompt: null,
        pendingUpload: false,
        remoteChanged: false,
      };
      sessions.set(id, s);
      emit(s);
      void simulateEditor(s);
      return structuredClone(s.info);
    },
    async editList() {
      return [...sessions.values()].map((s) => structuredClone(s.info));
    },
    async editClose(id, force) {
      const s = get(id);
      s.prompt?.abort();
      s.prompt = null;
      if (s.info.dirty && !force) {
        set(s, { state: "stopped" });
        return s.info.localPath;
      }
      sessions.delete(id);
      set(s, { state: "closed" });
      return null;
    },
    async editRetry(id) {
      const s = get(id);
      if (s.info.state === "stopped") s.info = { ...s.info, state: "failed", message: "" } as EditSession;
      if (s.remoteChanged) void conflict(s);
      else if (s.info.dirty) void upload(s);
    },
    async editReveal(id) {
      get(id);
    },
    async openLocal() {},
    async dragoutPrepare(sources) {
      const id = uid();
      const home = await deps.files.methods.fsHome(null);
      const dir = `${home}/.cache/patchbay/dragout/${id}`;
      deps.files.ensureDir(null, dir);
      const transfer = await deps.files.methods.transferStart({
        sources,
        destination: { hostId: null, path: dir },
        background: true,
        maxBytes: MAX_DRAGOUT,
      });
      dragouts.set(id, { transfer: transfer.id, dir });
      return { id, transfer, paths: sources.map((s) => `${dir}/${basename(s.path)}`) };
    },
    async dragoutRelease(id) {
      const d = dragouts.get(id);
      if (!d) return;
      dragouts.delete(id);
      await deps.files.methods.transferCancel(d.transfer).catch(() => undefined);
      await deps.files.methods.fsDelete(null, [d.dir]).catch(() => undefined);
    },
    async startNativeDrag(_paths, _icon, onEnd) {
      setTimeout(() => onEnd(true), 300);
    },
  };

  return {
    methods,
    hostConnected(hostId) {
      for (const s of sessions.values()) {
        if (s.info.hostId === hostId && s.pendingUpload && s.info.state === "failed") void upload(s);
      }
    },
  };
}
