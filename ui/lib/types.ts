// Mirrors crates/patchbay-core/src/model.rs (serde camelCase). Keep in sync.

/** `cfg:<alias>` for ssh_config hosts, `ts:<node id>` for Tailscale devices. */
export type HostId = string;
/** sshConfig: a `Host` in ~/.ssh/config or its includes (see `Host.config`). */
export type HostSource = "sshConfig" | "tailscale";

export interface Host {
  id: HostId;
  name: string;
  source: HostSource;
  hostname: string;
  port: number;
  user: string | null;
  identityFiles: string[];
  proxyJump: string | null;
  group: string | null;
  tags: string[];
  /** Set for ssh_config hosts. */
  config?: ConfigInfo;
  /** Set for Tailscale devices. */
  tailscale?: TailscaleInfo;
}

/** Where an ssh_config host is defined and whether Patchbay can edit it. */
export interface ConfigInfo {
  /** The file with the host's `Host` line. */
  file: string;
  /** 1-based line of the `Host` line. */
  line: number;
  /** Why Patchbay can't edit the host; null if it can. */
  readOnly: string | null;
  /** The options written in the host's own block (what the editor edits). */
  written: WrittenOptions;
  /** Options in the block that ssh ignores because an earlier block sets them. */
  overridden: string[];
  /** `ssh <name>` works: the file is (included from) ~/.ssh/config. */
  visibleToSsh: boolean;
}

export interface WrittenOptions {
  hostname: string | null;
  port: number | null;
  user: string | null;
  identityFiles: string[];
  proxyJump: string | null;
}

export interface TailscaleInfo {
  online: boolean;
  /** As reported by Tailscale: "linux", "windows", "macOS", ... */
  os: string;
  dnsName: string | null;
  ips: string[];
  /** The device runs Tailscale SSH. */
  ssh: boolean;
  /** Login name of the owner; null for tagged devices. */
  owner: string | null;
  /** Users to offer when asking whom to log in as, best guess first. */
  userSuggestions: string[];
}

export interface HostDraft {
  name: string;
  hostname: string;
  port: number | null;
  user: string | null;
  identityFiles: string[];
  proxyJump: string | null;
  group: string | null;
  tags: string[];
}

export interface KbdPrompt {
  text: string;
  echo: boolean;
}

export type Prompt =
  | { kind: "hostKeyUnknown"; host: string; port: number; keyType: string; fingerprint: string }
  | {
      kind: "hostKeyChanged";
      host: string;
      port: number;
      keyType: string;
      fingerprint: string;
      knownHostsFile: string;
      line: number;
    }
  | { kind: "password"; user: string; host: string; retry: boolean }
  | { kind: "keyPassphrase"; keyPath: string; retry: boolean }
  | {
      kind: "keyboardInteractive";
      user: string;
      host: string;
      name: string;
      instructions: string;
      prompts: KbdPrompt[];
    }
  | {
      kind: "fileConflict";
      transferId: string;
      path: string;
      source: FileStat;
      existing: FileStat;
      canResume: boolean;
    }
  | {
      /** A file being edited locally also changed on the server. */
      kind: "editConflict";
      sessionId: string;
      remotePath: string;
      /** The server's current version. */
      remote: FileStat;
      /** The local copy that was saved. */
      local: FileStat;
    };

export type PromptResponse =
  | { kind: "accept" }
  | { kind: "reject" }
  | { kind: "secret"; value: string; remember: boolean }
  | { kind: "answers"; answers: string[] }
  | { kind: "conflict"; resolution: ConflictResolution; applyToAll: boolean }
  | { kind: "editConflict"; resolution: EditConflictResolution }
  | { kind: "cancel" };

export interface PromptPayload {
  id: string;
  hostId: HostId | null;
  prompt: Prompt;
}

export type RemoteOs = "unknown" | "linux" | "macOs" | "bsd" | "otherUnix" | "windows";

export interface RemoteCapabilities {
  sftp: boolean;
  exec: boolean;
  os: RemoteOs;
  home: string | null;
}

export type ConnectionState =
  | { state: "connecting"; hop: string | null }
  | { state: "authenticating" }
  | { state: "connected"; capabilities: RemoteCapabilities }
  | { state: "reconnecting"; attempt: number }
  | { state: "disconnected"; reason: string | null };

export type ConflictResolution = "overwrite" | "skip" | "rename" | "overwriteIfNewer" | "resume";

/**
 * - overwrite: upload the local copy over the server's version
 * - discardLocal: throw the local changes away, download the server's version
 * - keepLocal: stop syncing; the local copy stays where it is
 */
export type EditConflictResolution = "overwrite" | "discardLocal" | "keepLocal";

export type CoreEvent =
  | ({ type: "connection"; hostId: HostId } & ConnectionState)
  | { type: "hostsChanged" }
  | { type: "authBanner"; hostId: HostId; message: string }
  | ({ type: "transfer" } & TransferInfo)
  | {
      type: "search";
      searchId: string;
      entries: FileEntry[];
      done: boolean;
      error: string | null;
    }
  | {
      /** The shell exited or its connection went away; all output was delivered first. */
      type: "terminalExit";
      terminalId: string;
      exitCode: number | null;
      reason: string | null;
    }
  | { type: "snippetsChanged" }
  | ({ type: "portForward" } & PortForwardStatus)
  | { type: "portForwardsChanged" }
  | ({ type: "settingsChanged" } & AppSettings)
  /** An edit session changed; state "closed" = it is gone. */
  | ({ type: "edit" } & EditSession);

// ---------------------------------------------------------------------------
// Files and transfers. Remote paths are POSIX-style (Windows OpenSSH servers
// use `/C:/Users/...`); local paths are native. `hostId: null` = this machine.
// ---------------------------------------------------------------------------

export interface FileRef {
  hostId: HostId | null;
  path: string;
}

export type FileKind = "file" | "dir" | "symlink" | "other";

export interface FileEntry {
  name: string;
  path: string;
  kind: FileKind;
  linkTarget: string | null;
  targetKind: FileKind | null;
  size: number;
  /** Unix seconds. */
  modified: number | null;
  /** Permission bits (0o7777 mask). */
  permissions: number | null;
  owner: string | null;
  group: string | null;
  hidden: boolean;
}

export interface FileStat {
  size: number;
  modified: number | null;
}

export interface DirListing {
  path: string;
  parent: string | null;
  entries: FileEntry[];
}

export interface TransferRequest {
  sources: FileRef[];
  destination: FileRef;
  /** Starts at once regardless of the concurrency limit (drag-out copies). */
  background?: boolean;
  /** Fail after scanning if the sources add up to more than this. */
  maxBytes?: number | null;
}

export type TransferState =
  | { state: "queued" }
  | { state: "scanning" }
  | { state: "running" }
  | { state: "paused" }
  | { state: "waitingForUser" }
  | { state: "done" }
  | { state: "failed"; message: string; resumable: boolean }
  | { state: "cancelled" };

export type TransferInfo = {
  id: string;
  label: string;
  /** Not requested by the user (a drag-out copy); hidden unless it takes a while. */
  background: boolean;
  sourceHost: HostId | null;
  destination: FileRef;
  totalBytes: number | null;
  doneBytes: number;
  totalFiles: number | null;
  doneFiles: number;
  skippedFiles: number;
  currentFile: string | null;
  bytesPerSec: number;
  /** Unix milliseconds. */
  createdAt: number;
  finishedAt: number | null;
} & TransferState;

/** A local copy of remote entries for dragging them out to the OS. */
export interface DragOut {
  id: string;
  /** The background transfer making the copy. */
  transfer: TransferInfo;
  /** Local paths, one per source; complete once the transfer is done. */
  paths: string[];
}

// ---------------------------------------------------------------------------
// Editing remote files in local apps.
// ---------------------------------------------------------------------------

export type EditState =
  /** Downloading the server's version. */
  | { state: "syncing" }
  /** The local copy matches the server as of `at` (Unix ms). */
  | { state: "synced"; at: number }
  | { state: "uploading" }
  /** Retried on the next save, on request, and when the host reconnects. */
  | { state: "failed"; message: string }
  /** Waiting for an answer to an `editConflict` prompt. */
  | { state: "conflict" }
  /** Not syncing; the local copy (with unsynced changes) is kept. Retry resumes. */
  | { state: "stopped" }
  /** Gone (last event for the session). */
  | { state: "closed" };

export type EditSession = {
  id: string;
  hostId: HostId | null;
  remotePath: string;
  /** Local copy (native path), named like the remote file. */
  localPath: string;
  /** The local copy has changes the server doesn't have. */
  dirty: boolean;
  /** Left behind by a previous run with unsynced changes. */
  recovered: boolean;
  /** Last successful upload (Unix ms). */
  lastUpload: number | null;
} & EditState;

export type ErrorKind =
  | "unknownHost"
  | "readOnlyHost"
  | "hostKeyRejected"
  | "authFailed"
  | "userRequired"
  | "cancelled"
  | "notConnected"
  | "timeout"
  | "io"
  | "notFound"
  | "permissionDenied"
  | "alreadyExists"
  | "unsupported"
  | "sftp"
  | "ssh"
  | "other";

export interface CommandError {
  kind: ErrorKind;
  message: string;
}

// ---------------------------------------------------------------------------
// Terminals and snippets.
// ---------------------------------------------------------------------------

export interface TerminalSpec {
  /** null = local shell on this machine. */
  hostId: HostId | null;
  /** Start directory ("open terminal here"); null = home. */
  cwd: string | null;
  cols: number;
  rows: number;
}

export interface TerminalInfo {
  id: string;
  hostId: HostId | null;
  /** Shell that was started, for display. */
  shell: string;
}

export interface Snippet {
  id: string;
  name: string;
  command: string;
  /** Empty = available everywhere. */
  hostIds: HostId[];
  /** Press Enter after inserting. */
  run: boolean;
}

export interface SnippetDraft {
  name: string;
  command: string;
  hostIds: HostId[];
  run: boolean;
}

// ---------------------------------------------------------------------------
// Port forwarding.
// ---------------------------------------------------------------------------

/**
 * - local: listen on this machine, connect to target as seen from hostId (-L)
 * - remote: listen on hostId, connect to target from this machine, or from
 *   viaHostId when set (machine-to-machine) (-R)
 * - dynamic: SOCKS5 proxy on this machine, connecting out from hostId (-D)
 */
/** local = -L, remote = -R, dynamic = -D (SOCKS here), remoteDynamic = -R port (SOCKS on the server). */
export type PortForwardKind = "local" | "remote" | "dynamic" | "remoteDynamic";

export interface PortForward {
  /** `fwd:<uuid>` (app) or `cfg:<alias>:<n>` (ssh_config, read-only). */
  id: string;
  name: string;
  source: "app" | "sshConfig";
  kind: PortForwardKind;
  hostId: HostId;
  /** null = loopback. Local/dynamic: on this machine; remote: on hostId. */
  bindAddress: string | null;
  /** 0 = any free port (local/dynamic only). */
  bindPort: number;
  targetHost: string | null;
  targetPort: number | null;
  viaHostId: HostId | null;
  autoStart: boolean;
}

export interface PortForwardDraft {
  name: string;
  kind: PortForwardKind;
  hostId: HostId;
  bindAddress: string | null;
  bindPort: number;
  targetHost: string | null;
  targetPort: number | null;
  viaHostId: HostId | null;
  autoStart: boolean;
}

export type PortForwardState =
  | { state: "stopped" }
  | { state: "starting" }
  | { state: "active"; address: string }
  | { state: "reconnecting"; attempt: number; message: string }
  | { state: "failed"; message: string };

export type PortForwardStatus = {
  id: string;
  openConnections: number;
  totalConnections: number;
  bytesIn: number;
  bytesOut: number;
} & PortForwardState;

/** Settings the backend acts on. UI-only preferences stay in localStorage. */
export interface AppSettings {
  closeToTray: boolean;
  launchAtLogin: boolean;
  startMinimized: boolean;
  maxConcurrentTransfers: number;
  /** List the devices of the user's tailnet. */
  tailscale: boolean;
}

// ---------------------------------------------------------------- windows

/** Roles among the open windows (`windows-changed`), by window label. */
export interface WindowsInfo {
  /** Runs once-per-app work (update checks): the oldest open window. */
  leader: string | null;
  /** Most recently focused: gets prompts nobody asked for in particular. */
  active: string | null;
  count: number;
  /** The open windows, most recently focused first. */
  windows: WindowSummary[];
}

/** How a window describes itself to the others ("Move to window" menus). */
export interface WindowSummary {
  label: string;
  /** Title of its active tab. */
  title: string | null;
  tabs: number;
  /** Its place among the open windows in the order they were opened (1: the oldest). */
  number: number;
}

/** A point on screen, in CSS pixels. */
export interface ScreenPoint {
  x: number;
  y: number;
}

/** A tab dragged out of its window, as other windows see it while it hovers over them. */
export interface DraggedTab {
  id: string;
  kind: string;
  hostId: HostId | null;
  /** As its tab strip shows it. */
  title: string;
}

/** The window under a tab dragged out of another one, and the pointer in its viewport (CSS pixels). */
export interface TabDragOver {
  label: string;
  x: number;
  y: number;
}

/** A tab of another window dragged over this one, at (`x`, `y`) in this viewport. */
export interface TabDragIn {
  tab: DraggedTab;
  x: number;
  y: number;
}

export interface WindowInfo extends WindowsInfo {
  /** This window's label. */
  label: string;
}

/** What was saved for a window. */
export interface WindowSession {
  /** Its tabs (`ui/lib/stores/session.svelte.ts` format), unless they aren't restored. */
  session: unknown;
  /** Its sidebar is hidden (kept even when tabs aren't restored). */
  sidebarHidden: boolean;
  /** Nothing saved yet: take over the single-window session of earlier versions. */
  migrate: boolean;
}
