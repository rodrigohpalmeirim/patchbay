// Typed wrappers around the Tauri commands in src-tauri/src/commands.rs.
// Outside Tauri (plain `npm run dev` in a browser) a mock backend is used so
// the UI can be developed and previewed without the Rust side.

import type {
  AppSettings,
  ConnectionState,
  CoreEvent,
  DirListing,
  DragOut,
  EditSession,
  FileEntry,
  FileRef,
  Host,
  HostDraft,
  HostId,
  PromptPayload,
  SshSharing,
  PortForward,
  PortForwardDraft,
  PortForwardStatus,
  PromptResponse,
  Snippet,
  SnippetDraft,
  TerminalInfo,
  TerminalSpec,
  TransferInfo,
  TransferRequest,
} from "./types";

export type Unlisten = () => void;

const isLinux = typeof navigator !== "undefined" && /Linux/.test(navigator.platform) && !/Android/.test(navigator.userAgent);

export interface Backend {
  listHosts(): Promise<Host[]>;
  hostWarnings(): Promise<string[]>;
  reloadHosts(): Promise<Host[]>;
  createHost(draft: HostDraft): Promise<Host>;
  updateHost(id: HostId, draft: HostDraft): Promise<Host>;
  deleteHost(id: HostId): Promise<void>;
  /** Old id -> current id of renamed hosts. */
  hostRenames(): Promise<Record<HostId, HostId>>;
  sshSharing(): Promise<SshSharing>;
  /** Adds or removes `Include patchbay.conf` in ~/.ssh/config. */
  setSshSharing(shared: boolean): Promise<SshSharing>;
  /** Current state of every known connection, e.g. after a webview reload. */
  listConnections(): Promise<Record<HostId, ConnectionState>>;
  /**
   * Resolves once connected; rejects with a CommandError on failure.
   * `user` overrides the login user; Tailscale devices without a known user
   * reject with `userRequired`.
   */
  connect(id: HostId, user?: string): Promise<void>;
  disconnect(id: HostId): Promise<void>;
  respondPrompt(id: string, response: PromptResponse): Promise<boolean>;

  // Files. `hostId: null` = local machine. Remote commands connect on demand
  // only if the host is already connected; otherwise they reject with
  // `notConnected`.
  /** Home directory (remote: SFTP realpath of "."). */
  fsHome(hostId: HostId | null): Promise<string>;
  fsList(hostId: HostId | null, path: string): Promise<DirListing>;
  fsStat(hostId: HostId | null, path: string): Promise<FileEntry>;
  fsMkdir(hostId: HostId | null, path: string): Promise<void>;
  fsCreateFile(hostId: HostId | null, path: string): Promise<void>;
  /** Rename / move within one machine. */
  fsRename(hostId: HostId | null, from: string, to: string): Promise<void>;
  /** Recursive delete. */
  fsDelete(hostId: HostId | null, paths: string[]): Promise<void>;
  fsChmod(hostId: HostId | null, paths: string[], mode: number, recursive: boolean): Promise<void>;
  /** Owner/group by name or numeric id; names need a remote shell. */
  fsChown(
    hostId: HostId | null,
    paths: string[],
    owner: string | null,
    group: string | null,
    recursive: boolean,
  ): Promise<void>;
  /** Up to `maxBytes` from the start of a file (for previews). */
  fsRead(hostId: HostId | null, path: string, maxBytes: number): Promise<Uint8Array>;

  // Transfers: progress arrives as `transfer` core events, conflicts as
  // `fileConflict` prompts.
  transferStart(request: TransferRequest): Promise<TransferInfo>;
  transferList(): Promise<TransferInfo[]>;
  transferPause(id: string): Promise<void>;
  transferResume(id: string): Promise<void>;
  transferCancel(id: string): Promise<void>;
  /** Restart a failed/cancelled transfer, continuing partial files. */
  transferRetry(id: string): Promise<void>;
  /** Forget finished, failed and cancelled transfers. */
  transferClearFinished(): Promise<void>;

  // Editing remote files in local apps: session changes arrive as `edit`
  // core events, conflicts as `editConflict` prompts.
  /**
   * Downloads a remote file into a temporary copy, opens it with the default
   * app and uploads it on every save. Reuses the running session of the file.
   */
  editOpen(hostId: HostId, path: string): Promise<EditSession>;
  editList(): Promise<EditSession[]>;
  /**
   * Stops syncing and deletes the copy. Without `force`, a copy with
   * unsynced changes is kept (session "stopped") and its path returned.
   */
  editClose(id: string, force: boolean): Promise<string | null>;
  /** Sync now, ask again about a conflict, or resume a stopped session. */
  editRetry(id: string): Promise<void>;
  /** Shows the local copy in the system file manager. */
  editReveal(id: string): Promise<void>;
  /** Opens a local file with the system's default app. */
  openLocal(path: string): Promise<void>;

  // Dragging entries out to the OS.
  /** Starts a background copy of remote entries into a temporary folder. */
  dragoutPrepare(sources: FileRef[]): Promise<DragOut>;
  /** The drag ended inside the window: stop the copy and delete it. */
  dragoutRelease(id: string): Promise<void>;
  /**
   * Starts a native OS drag of local paths (the pointer button must be
   * held). `onEnd` reports whether something accepted the drop.
   */
  startNativeDrag(paths: string[], icon: string, onEnd: (dropped: boolean) => void): Promise<void>;

  // Search: results stream in as `search` core events.
  /** Case-insensitive name search under `root`; `query` may use * and ? wildcards. */
  searchStart(hostId: HostId | null, root: string, query: string): Promise<string>;
  searchCancel(searchId: string): Promise<void>;

  // Terminals. Output bytes arrive on `onData` in order; a `terminalExit`
  // core event follows the last chunk. Remote terminals require a connected
  // host (`notConnected` otherwise).
  terminalOpen(spec: TerminalSpec, onData: (data: Uint8Array) => void): Promise<TerminalInfo>;
  /** Keyboard input / pasted text. */
  terminalWrite(id: string, data: string): Promise<void>;
  terminalResize(id: string, cols: number, rows: number): Promise<void>;
  /**
   * Flow control: report `bytes` of output as rendered. Once the UI starts
   * acknowledging, the backend pauses while more than ~1 MiB is unacknowledged.
   */
  terminalAck(id: string, bytes: number): Promise<void>;
  /** Close the shell (no-op if already exited). */
  terminalClose(id: string): Promise<void>;

  // Snippets (saved commands), persisted by the backend.
  snippetsList(): Promise<Snippet[]>;
  snippetCreate(draft: SnippetDraft): Promise<Snippet>;
  snippetUpdate(id: string, draft: SnippetDraft): Promise<Snippet>;
  snippetDelete(id: string): Promise<void>;

  // Port forwards. Starting connects the host if needed (prompts may
  // appear); status changes arrive as `portForward` core events.
  forwardsList(): Promise<PortForward[]>;
  forwardStatuses(): Promise<PortForwardStatus[]>;
  forwardCreate(draft: PortForwardDraft): Promise<PortForward>;
  /** Restarts the forward if it was running. */
  forwardUpdate(id: string, draft: PortForwardDraft): Promise<PortForward>;
  forwardDelete(id: string): Promise<void>;
  forwardStart(id: string): Promise<PortForwardStatus>;
  forwardStop(id: string): Promise<void>;

  // Backend settings.
  settingsGet(): Promise<AppSettings>;
  settingsSet(settings: AppSettings): Promise<AppSettings>;
  onCoreEvent(handler: (event: CoreEvent) => void): Promise<Unlisten>;
  onPrompt(handler: (prompt: PromptPayload) => void): Promise<Unlisten>;
  /** The backend abandoned a pending prompt (attempt aborted); dismiss it. */
  onPromptClosed(handler: (id: string) => void): Promise<Unlisten>;
}

export const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function tauriBackend(): Promise<Backend> {
  const { invoke, Channel } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");
  return {
    listHosts: () => invoke("list_hosts"),
    hostWarnings: () => invoke("host_warnings"),
    reloadHosts: () => invoke("reload_hosts"),
    createHost: (draft) => invoke("create_host", { draft }),
    updateHost: (id, draft) => invoke("update_host", { id, draft }),
    deleteHost: (id) => invoke("delete_host", { id }),
    hostRenames: () => invoke("host_renames"),
    sshSharing: () => invoke("ssh_sharing"),
    setSshSharing: (shared) => invoke("set_ssh_sharing", { shared }),
    listConnections: () => invoke("list_connections"),
    connect: (id, user) => invoke("connect", { id, user: user ?? null }),
    disconnect: (id) => invoke("disconnect", { id }),
    respondPrompt: (id, response) => invoke("respond_prompt", { id, response }),
    fsHome: (hostId) => invoke("fs_home", { hostId }),
    fsList: (hostId, path) => invoke("fs_list", { hostId, path }),
    fsStat: (hostId, path) => invoke("fs_stat", { hostId, path }),
    fsMkdir: (hostId, path) => invoke("fs_mkdir", { hostId, path }),
    fsCreateFile: (hostId, path) => invoke("fs_create_file", { hostId, path }),
    fsRename: (hostId, from, to) => invoke("fs_rename", { hostId, from, to }),
    fsDelete: (hostId, paths) => invoke("fs_delete", { hostId, paths }),
    fsChmod: (hostId, paths, mode, recursive) =>
      invoke("fs_chmod", { hostId, paths, mode, recursive }),
    fsChown: (hostId, paths, owner, group, recursive) =>
      invoke("fs_chown", { hostId, paths, owner, group, recursive }),
    fsRead: async (hostId, path, maxBytes) =>
      new Uint8Array(await invoke<ArrayBuffer>("fs_read", { hostId, path, maxBytes })),
    transferStart: (request) => invoke("transfer_start", { request }),
    transferList: () => invoke("transfer_list"),
    transferPause: (id) => invoke("transfer_pause", { id }),
    transferResume: (id) => invoke("transfer_resume", { id }),
    transferCancel: (id) => invoke("transfer_cancel", { id }),
    transferRetry: (id) => invoke("transfer_retry", { id }),
    transferClearFinished: () => invoke("transfer_clear_finished"),
    editOpen: (hostId, path) => invoke("edit_open", { hostId, path }),
    editList: () => invoke("edit_list"),
    editClose: (id, force) => invoke("edit_close", { id, force }),
    editRetry: (id) => invoke("edit_retry", { id }),
    editReveal: (id) => invoke("edit_reveal", { id }),
    openLocal: (path) => invoke("open_local", { path }),
    dragoutPrepare: (sources) => invoke("dragout_prepare", { sources }),
    dragoutRelease: (id) => invoke("dragout_release", { id }),
    startNativeDrag: async (paths, icon, onEnd) => {
      if (isLinux) {
        // tauri-plugin-drag's GTK backend loses the data before the drop
        // target asks for it; see `dragout_start_linux`.
        const channel = new Channel<boolean>();
        channel.onmessage = onEnd;
        const png = Array.from(atob(icon.slice(icon.indexOf(",") + 1)), (c) => c.charCodeAt(0));
        return invoke("dragout_start_linux", { paths, icon: png, onEnd: channel });
      }
      const { startDrag } = await import("@crabnebula/tauri-plugin-drag");
      // The Rust side reports "Dropped" or "Cancel".
      await startDrag({ item: paths, icon, mode: "copy" }, (e) => onEnd(e.result === "Dropped"));
    },
    searchStart: (hostId, root, query) => invoke("search_start", { hostId, root, query }),
    searchCancel: (searchId) => invoke("search_cancel", { searchId }),
    terminalOpen: async (spec, onData) => {
      const channel = new Channel<ArrayBuffer>();
      channel.onmessage = (chunk) => onData(new Uint8Array(chunk));
      return invoke("terminal_open", { spec, onData: channel });
    },
    terminalWrite: (id, data) => invoke("terminal_write", { id, data }),
    terminalResize: (id, cols, rows) => invoke("terminal_resize", { id, cols, rows }),
    terminalAck: (id, bytes) => invoke("terminal_ack", { id, bytes }),
    terminalClose: (id) => invoke("terminal_close", { id }),
    snippetsList: () => invoke("snippets_list"),
    snippetCreate: (draft) => invoke("snippet_create", { draft }),
    snippetUpdate: (id, draft) => invoke("snippet_update", { id, draft }),
    snippetDelete: (id) => invoke("snippet_delete", { id }),
    forwardsList: () => invoke("forwards_list"),
    forwardStatuses: () => invoke("forward_statuses"),
    forwardCreate: (draft) => invoke("forward_create", { draft }),
    forwardUpdate: (id, draft) => invoke("forward_update", { id, draft }),
    forwardDelete: (id) => invoke("forward_delete", { id }),
    forwardStart: (id) => invoke("forward_start", { id }),
    forwardStop: (id) => invoke("forward_stop", { id }),
    settingsGet: () => invoke("settings_get"),
    settingsSet: (settings) => invoke("settings_set", { settings }),
    onCoreEvent: (handler) => listen<CoreEvent>("core-event", (e) => handler(e.payload)),
    onPrompt: (handler) => listen<PromptPayload>("prompt", (e) => handler(e.payload)),
    onPromptClosed: (handler) =>
      listen<{ id: string }>("prompt-closed", (e) => handler(e.payload.id)),
  };
}

let backend: Promise<Backend> | undefined;

export function getBackend(): Promise<Backend> {
  backend ??= isTauri ? tauriBackend() : import("./mock").then((m) => m.createMockBackend());
  return backend;
}
