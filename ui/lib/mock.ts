// In-memory stand-in for the Rust backend, used when the UI runs in a plain
// browser (`npm run dev`). It mimics the real event flow closely enough to
// exercise every UI state: hop progress, host-key prompts, password and
// keyboard-interactive auth, SFTP-only servers, failures, and CRUD.
//
// Scenario per host (see `SCENARIOS`):
//   bastion        key auth, connects straight away
//   db-internal    ProxyJump through bastion
//   legacy-nas     host key CHANGED (possible MITM)
//   build-mac      encrypted key -> passphrase prompt
//   raspberrypi    unknown host key, then password (type "wrong" to see retry)
//   media-share    SFTP-only server (no shell)
//   prod-web-01    keyboard-interactive 2FA
//   staging-api    authentication fails
//   win-buildbox   Windows OpenSSH
//
// Files, transfers and search live in mock/ (files.ts = commands, trees.ts =
// per-host filesystem contents, vfs.ts = model, content.ts = file contents).
// Remote fs calls reject with `notConnected` until the host is connected.
// Demo material:
//   local ~/Projects/big-dir, bastion /var/log/archive    2000 entries each
//   local ~/Pictures                                       png/jpg/webp/gif/svg/bmp/ico,
//                                                          panorama-raw.png is 24 MiB
//   local ~/Documents -> bastion ~/docs                   conflicts (architecture.pdf
//                                                          is a partial -> Resume)
//   local ~/Videos/holiday.mp4 -> media-share /upload     partial -> Resume
//   local ~/Downloads/big-dataset.tar.zst                 fails once at 40%, retry resumes
//   /root everywhere, win /C:/Windows/System32/config      permission denied
//   media-share                                            writable only under /upload
//
// Terminals (mock/shell.ts): a small fake shell per terminal on the same
// filesystem; type `help`. `drop-connection` in a remote shell simulates a
// network failure. Snippets live in memory (three samples below).
//
// Port forwards and backend settings: mock/forwards.ts (scripted states are
// listed there). Editing in local apps and drag-out: mock/edit.ts.

import type { Backend, Unlisten } from "./api";
import { createEdits } from "./mock/edit";
import { createFiles } from "./mock/files";
import { createForwards } from "./mock/forwards";
import { createTerminals } from "./mock/shell";
import type {
  CommandError,
  ConnectionState,
  CoreEvent,
  ErrorKind,
  Host,
  HostDraft,
  HostId,
  Prompt,
  PromptPayload,
  PromptResponse,
  RemoteCapabilities,
  Snippet,
  SnippetDraft,
} from "./types";

const HOME = "~";

const INITIAL_HOSTS: Host[] = [
  {
    id: "cfg:bastion",
    name: "bastion",
    source: "sshConfig",
    hostname: "bastion.example.net",
    port: 22,
    user: "ops",
    identityFiles: [`${HOME}/.ssh/id_ed25519`],
    proxyJump: null,
    group: null,
    tags: [],
  },
  {
    id: "cfg:db-internal",
    name: "db-internal",
    source: "sshConfig",
    hostname: "10.0.4.12",
    port: 22,
    user: "postgres",
    identityFiles: [`${HOME}/.ssh/id_ed25519`],
    proxyJump: "bastion",
    group: null,
    tags: [],
  },
  {
    id: "cfg:legacy-nas",
    name: "legacy-nas",
    source: "sshConfig",
    hostname: "nas.lan",
    port: 2222,
    user: "admin",
    identityFiles: [],
    proxyJump: null,
    group: null,
    tags: [],
  },
  {
    id: "cfg:build-mac",
    name: "build-mac",
    source: "sshConfig",
    hostname: "build-mac.office.example.com",
    port: 22,
    user: "ci",
    identityFiles: [`${HOME}/.ssh/id_rsa_build`],
    proxyJump: null,
    group: null,
    tags: [],
  },
  {
    id: "app:6f1c2a90-raspberrypi",
    name: "raspberrypi",
    source: "app",
    hostname: "192.168.1.40",
    port: 22,
    user: "pi",
    identityFiles: [],
    proxyJump: null,
    group: "Homelab",
    tags: ["arm", "pi"],
  },
  {
    id: "app:0b7e44d1-media-share",
    name: "media-share",
    source: "app",
    hostname: "media.home.arpa",
    port: 22,
    user: "media",
    identityFiles: [`${HOME}/.ssh/id_ed25519_media`],
    proxyJump: null,
    group: "Homelab",
    tags: ["sftp", "storage"],
  },
  {
    id: "app:9a3d5e2f-prod-web-01",
    name: "prod-web-01",
    source: "app",
    hostname: "web01.prod.example.com",
    port: 22,
    user: "deploy",
    identityFiles: [],
    proxyJump: null,
    group: "Work",
    tags: ["prod", "nginx"],
  },
  {
    id: "app:47c8b1aa-staging-api",
    name: "staging-api",
    source: "app",
    hostname: "api.staging.example.com",
    port: 2200,
    user: "deploy",
    identityFiles: [`${HOME}/.ssh/id_ed25519_work`],
    proxyJump: "bastion",
    group: "Work",
    tags: ["staging", "api"],
  },
  {
    id: "app:d2e9f310-win-buildbox",
    name: "win-buildbox",
    source: "app",
    hostname: "10.20.0.15",
    port: 22,
    user: "rodrigo",
    identityFiles: [],
    proxyJump: null,
    group: null,
    tags: ["windows"],
  },
];

const WARNINGS = [
  "~/.ssh/config:42: unsupported option `ControlMaster` (ignored)",
  "~/.ssh/config.d/work:7: Include pattern matched no files: ~/.ssh/config.d/legacy/*",
];

const LINUX: RemoteCapabilities = { sftp: true, exec: true, os: "linux", home: null };

/** Scripted connection behaviour, keyed by host name. */
type Scenario =
  | { kind: "ok"; caps: RemoteCapabilities }
  | { kind: "unknownKeyThenPassword"; caps: RemoteCapabilities }
  | { kind: "changedKey"; caps: RemoteCapabilities }
  | { kind: "passphrase"; caps: RemoteCapabilities }
  | { kind: "twoFactor"; caps: RemoteCapabilities }
  | { kind: "authFailed" };

const SCENARIOS: Record<string, Scenario> = {
  bastion: { kind: "ok", caps: { ...LINUX, home: "/home/ops" } },
  "db-internal": { kind: "ok", caps: { ...LINUX, home: "/var/lib/postgresql" } },
  "legacy-nas": { kind: "changedKey", caps: { ...LINUX, os: "bsd", home: "/home/admin" } },
  "build-mac": { kind: "passphrase", caps: { sftp: true, exec: true, os: "macOs", home: "/Users/ci" } },
  raspberrypi: { kind: "unknownKeyThenPassword", caps: { ...LINUX, home: "/home/pi" } },
  "media-share": { kind: "ok", caps: { sftp: true, exec: false, os: "unknown", home: "/" } },
  "prod-web-01": { kind: "twoFactor", caps: { ...LINUX, home: "/home/deploy" } },
  "staging-api": { kind: "authFailed" },
  "win-buildbox": {
    kind: "ok",
    caps: { sftp: true, exec: true, os: "windows", home: "C:\\Users\\rodrigo" },
  },
};

const FINGERPRINTS: Record<string, string> = {
  raspberrypi: "SHA256:q7Xv4mJ0r2nLkA9cWbE5tZ1yHf3uPd8sGo6iKe2RjNw",
  "legacy-nas": "SHA256:Zt3Bq9xLm1VcR7sNw2YkPa5eHu8gDf4jTo0iKb6QvXc",
};

class Cancelled extends Error {}

function err(kind: ErrorKind, message: string): CommandError {
  return { kind, message };
}

function delay(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal.aborted) return reject(new Cancelled());
    const t = setTimeout(resolve, ms);
    signal.addEventListener(
      "abort",
      () => {
        clearTimeout(t);
        reject(new Cancelled());
      },
      { once: true },
    );
  });
}

function uuid(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) return crypto.randomUUID();
  return Math.random().toString(16).slice(2) + Date.now().toString(16);
}

const INITIAL_SNIPPETS: Snippet[] = [
  { id: "snip-disk", name: "Disk usage", command: "df -h", hostIds: [], run: true },
  {
    id: "snip-auth",
    name: "Recent SSH logins",
    command: "tail -n 15 /var/log/auth.log",
    hostIds: ["cfg:bastion"],
    run: true,
  },
  {
    id: "snip-nginx",
    name: "Reload nginx",
    command: "sudo nginx -t && sudo systemctl reload nginx",
    hostIds: ["app:9a3d5e2f-prod-web-01", "app:47c8b1aa-staging-api"],
    run: false,
  },
];

export function createMockBackend(): Backend {
  let hosts: Host[] = structuredClone(INITIAL_HOSTS);
  let snippets: Snippet[] = structuredClone(INITIAL_SNIPPETS);
  const eventHandlers = new Set<(e: CoreEvent) => void>();
  const promptHandlers = new Set<(p: PromptPayload) => void>();
  const promptClosedHandlers = new Set<(id: string) => void>();
  const states = new Map<HostId, ConnectionState>();
  const pending = new Map<string, (r: PromptResponse) => void>();
  const sessions = new Map<HostId, { abort: AbortController; connected: boolean }>();
  /** In-flight connect() calls, so forwards can join an attempt instead of restarting it. */
  const connecting = new Map<HostId, Promise<void>>();

  const emit = (e: CoreEvent) => {
    if (e.type === "connection") {
      const { type: _type, hostId, ...state } = e;
      if (state.state === "disconnected") states.delete(hostId);
      else states.set(hostId, state);
    }
    // Events arrive asynchronously in Tauri as well.
    queueMicrotask(() => eventHandlers.forEach((h) => h(structuredClone(e))));
  };

  const find = (id: HostId): Host => {
    const host = hosts.find((h) => h.id === id);
    if (!host) throw err("unknownHost", `unknown host: ${id}`);
    return host;
  };

  const ask = (hostId: HostId | null, prompt: Prompt, signal: AbortSignal): Promise<PromptResponse> =>
    new Promise((resolve, reject) => {
      const id = uuid();
      const onAbort = () => {
        pending.delete(id);
        promptClosedHandlers.forEach((h) => h(id));
        reject(new Cancelled());
      };
      signal.addEventListener("abort", onAbort, { once: true });
      pending.set(id, (r) => {
        signal.removeEventListener("abort", onAbort);
        resolve(r);
      });
      const payload: PromptPayload = { id, hostId, prompt };
      setTimeout(() => promptHandlers.forEach((h) => h(structuredClone(payload))), 0);
    });

  const validate = (draft: HostDraft, selfId: HostId | null) => {
    if (!draft.name.trim()) throw err("other", "name must not be empty");
    if (!draft.hostname.trim()) throw err("other", "hostname must not be empty");
    const clash = hosts.find(
      (h) => h.id !== selfId && h.name.toLowerCase() === draft.name.trim().toLowerCase(),
    );
    if (clash) throw err("other", `a host named "${draft.name}" already exists`);
  };

  const fromDraft = (id: HostId, d: HostDraft): Host => ({
    id,
    name: d.name.trim(),
    source: "app",
    hostname: d.hostname.trim(),
    port: d.port ?? 22,
    user: d.user,
    identityFiles: [...d.identityFiles],
    proxyJump: d.proxyJump,
    group: d.group,
    tags: [...d.tags],
  });

  /** Walks the ProxyJump chain (by host name), first hop first. */
  const jumpChain = (host: Host, seen = new Set<string>()): string[] => {
    if (!host.proxyJump || seen.has(host.name)) return [];
    seen.add(host.name);
    const out: string[] = [];
    for (const part of host.proxyJump.split(",").map((s) => s.trim()).filter(Boolean)) {
      const hop = hosts.find((h) => h.name === part);
      if (hop) out.push(...jumpChain(hop, seen));
      out.push(part);
    }
    return out;
  };

  async function runConnect(host: Host, signal: AbortSignal): Promise<RemoteCapabilities> {
    const scenario: Scenario = SCENARIOS[host.name] ?? {
      kind: "ok",
      caps: { ...LINUX, home: `/home/${host.user ?? "user"}` },
    };
    const conn = (state: ConnectionState) =>
      emit({ type: "connection", hostId: host.id, ...state });

    for (const hop of jumpChain(host)) {
      conn({ state: "connecting", hop });
      await delay(700, signal);
    }
    conn({ state: "connecting", hop: null });
    await delay(800, signal);

    const user = host.user ?? "rodrigo";
    switch (scenario.kind) {
      case "changedKey": {
        const r = await ask(
          host.id,
          {
            kind: "hostKeyChanged",
            host: host.hostname,
            port: host.port,
            keyType: "ssh-ed25519",
            fingerprint: FINGERPRINTS[host.name],
            knownHostsFile: `${HOME}/.ssh/known_hosts`,
            line: 17,
          },
          signal,
        );
        if (r.kind === "cancel") throw new Cancelled();
        if (r.kind !== "accept")
          throw err(
            "hostKeyRejected",
            `host key for [${host.hostname}]:${host.port} does not match known_hosts:17`,
          );
        break;
      }
      case "unknownKeyThenPassword": {
        const r = await ask(
          host.id,
          {
            kind: "hostKeyUnknown",
            host: host.hostname,
            port: host.port,
            keyType: "ssh-ed25519",
            fingerprint: FINGERPRINTS[host.name],
          },
          signal,
        );
        if (r.kind === "cancel") throw new Cancelled();
        if (r.kind !== "accept")
          throw err("hostKeyRejected", `host key for ${host.hostname} was rejected`);
        break;
      }
    }

    conn({ state: "authenticating" });
    await delay(600, signal);

    switch (scenario.kind) {
      case "authFailed":
        await delay(500, signal);
        throw err(
          "authFailed",
          `authentication failed for ${user}@${host.hostname} (tried: publickey, password)`,
        );
      case "unknownKeyThenPassword": {
        let retry = false;
        for (let attempt = 0; ; attempt++) {
          const r = await ask(
            host.id,
            { kind: "password", user, host: host.hostname, retry },
            signal,
          );
          if (r.kind !== "secret") throw new Cancelled();
          await delay(500, signal);
          if (r.value && r.value !== "wrong") break;
          if (attempt >= 2)
            throw err("authFailed", `authentication failed for ${user}@${host.hostname}`);
          retry = true;
        }
        break;
      }
      case "passphrase": {
        let retry = false;
        for (;;) {
          const r = await ask(
            host.id,
            { kind: "keyPassphrase", keyPath: host.identityFiles[0] ?? "~/.ssh/id_rsa", retry },
            signal,
          );
          if (r.kind !== "secret") throw new Cancelled();
          await delay(400, signal);
          if (r.value && r.value !== "wrong") break;
          retry = true;
        }
        break;
      }
      case "twoFactor": {
        const r = await ask(
          host.id,
          {
            kind: "keyboardInteractive",
            user,
            host: host.hostname,
            name: "Two-factor authentication",
            instructions: "Enter your password and the 6-digit code from your authenticator app.",
            prompts: [
              { text: "Password: ", echo: false },
              { text: "Verification code: ", echo: true },
            ],
          },
          signal,
        );
        if (r.kind !== "answers") throw new Cancelled();
        await delay(700, signal);
        if (!/^\d{6}$/.test(r.answers[1]?.trim() ?? ""))
          throw err("authFailed", "keyboard-interactive authentication failed: invalid verification code");
        break;
      }
    }

    await delay(450, signal);
    return scenario.caps;
  }

  const files = createFiles({
    host: (id) => hosts.find((h) => h.id === id),
    hostByName: (name) => hosts.find((h) => h.name === name),
    isConnected: (id) => sessions.get(id)?.connected === true,
    emit,
    ask,
  });

  const edits = createEdits({
    isConnected: (id) => sessions.get(id)?.connected === true,
    emit,
    ask,
    files,
  });

  const terminals = createTerminals({
    host: (id) => hosts.find((h) => h.id === id),
    isConnected: (id) => sessions.get(id)?.connected === true,
    capabilities: (id) => {
      const host = hosts.find((h) => h.id === id);
      const s = host ? SCENARIOS[host.name] : undefined;
      return s && "caps" in s ? s.caps : host ? LINUX : null;
    },
    machine: (hostId) => files.machine(hostId),
    emit,
    dropConnection: (id, reason) => {
      const session = sessions.get(id);
      if (!session?.connected) return;
      sessions.delete(id);
      session.abort.abort();
      files.hostDisconnected(id);
      terminals.hostDisconnected(id, reason);
      emit({ type: "connection", hostId: id, state: "disconnected", reason });
      forwards.hostLost(id);
    },
  });

  const forwards = createForwards({
    host: (id) => hosts.find((h) => h.id === id),
    isConnected: (id) => sessions.get(id)?.connected === true,
    connect: (id) => connecting.get(id) ?? connect(id),
    canReconnectSilently: (id) => {
      const host = hosts.find((h) => h.id === id);
      return !!host && (SCENARIOS[host.name]?.kind ?? "ok") === "ok";
    },
    emit,
    err,
    uuid,
  });

  async function connect(id: HostId): Promise<void> {
    const host = find(id);
    const existing = sessions.get(id);
    if (existing?.connected) return;
    if (existing) existing.abort.abort();
    const session = { abort: new AbortController(), connected: false };
    sessions.set(id, session);
    const attempt = (async () => {
      try {
        const capabilities = await runConnect(host, session.abort.signal);
        session.connected = true;
        emit({ type: "connection", hostId: id, state: "connected", capabilities });
        forwards.hostConnected(id);
        edits.hostConnected(id);
      } catch (e) {
        if (sessions.get(id) === session) sessions.delete(id);
        if (e instanceof Cancelled) {
          emit({ type: "connection", hostId: id, state: "disconnected", reason: null });
          throw err("cancelled", "cancelled");
        }
        const ce = e as CommandError;
        emit({ type: "connection", hostId: id, state: "disconnected", reason: ce.message });
        throw ce;
      }
    })();
    connecting.set(id, attempt);
    const forget = () => {
      if (connecting.get(id) === attempt) connecting.delete(id);
    };
    attempt.then(forget, forget);
    return attempt;
  }

  const snippetFrom = (id: string, d: SnippetDraft): Snippet => {
    if (!d.name.trim()) throw err("other", "name must not be empty");
    if (!d.command.trim()) throw err("other", "command must not be empty");
    return { id, name: d.name.trim(), command: d.command, hostIds: [...new Set(d.hostIds)], run: d.run };
  };

  return {
    ...files.methods,
    ...edits.methods,
    ...terminals.methods,
    ...forwards.methods,
    async snippetsList() {
      return structuredClone(snippets);
    },
    async snippetCreate(draft) {
      const snippet = snippetFrom(`snip-${uuid()}`, draft);
      snippets = [...snippets, snippet];
      emit({ type: "snippetsChanged" });
      return structuredClone(snippet);
    },
    async snippetUpdate(id, draft) {
      if (!snippets.some((s) => s.id === id)) throw err("notFound", `no such snippet: ${id}`);
      const snippet = snippetFrom(id, draft);
      snippets = snippets.map((s) => (s.id === id ? snippet : s));
      emit({ type: "snippetsChanged" });
      return structuredClone(snippet);
    },
    async snippetDelete(id) {
      snippets = snippets.filter((s) => s.id !== id);
      emit({ type: "snippetsChanged" });
    },
    async listHosts() {
      return structuredClone(hosts);
    },
    async hostWarnings() {
      return [...WARNINGS];
    },
    async reloadHosts() {
      await new Promise((r) => setTimeout(r, 250));
      return structuredClone(hosts);
    },
    async createHost(draft) {
      validate(draft, null);
      const host = fromDraft(`app:${uuid()}`, draft);
      hosts = [...hosts, host];
      emit({ type: "hostsChanged" });
      return structuredClone(host);
    },
    async updateHost(id, draft) {
      const existing = find(id);
      if (existing.source !== "app")
        throw err("readOnlyHost", `${existing.name} is defined in ~/.ssh/config and is read-only`);
      validate(draft, id);
      const host = fromDraft(id, draft);
      hosts = hosts.map((h) => (h.id === id ? host : h));
      emit({ type: "hostsChanged" });
      return structuredClone(host);
    },
    async deleteHost(id) {
      const existing = find(id);
      if (existing.source !== "app")
        throw err("readOnlyHost", `${existing.name} is defined in ~/.ssh/config and is read-only`);
      sessions.get(id)?.abort.abort();
      sessions.delete(id);
      files.hostDisconnected(id);
      terminals.hostDisconnected(id, "host was deleted");
      forwards.hostDeleted(id);
      hosts = hosts.filter((h) => h.id !== id);
      emit({ type: "hostsChanged" });
    },
    connect,
    async disconnect(id) {
      const session = sessions.get(id);
      if (!session) return;
      sessions.delete(id);
      const wasConnected = session.connected;
      session.abort.abort();
      if (wasConnected) {
        files.hostDisconnected(id);
        terminals.hostDisconnected(id, "disconnected");
        emit({ type: "connection", hostId: id, state: "disconnected", reason: null });
      }
      forwards.hostDisconnected(id);
    },
    async respondPrompt(id, response) {
      const resolve = pending.get(id);
      if (!resolve) return false;
      pending.delete(id);
      resolve(response);
      return true;
    },
    async onCoreEvent(handler): Promise<Unlisten> {
      eventHandlers.add(handler);
      return () => eventHandlers.delete(handler);
    },
    async onPrompt(handler): Promise<Unlisten> {
      promptHandlers.add(handler);
      return () => promptHandlers.delete(handler);
    },
    async onPromptClosed(handler): Promise<Unlisten> {
      promptClosedHandlers.add(handler);
      return () => promptClosedHandlers.delete(handler);
    },
    async listConnections() {
      return Object.fromEntries(structuredClone([...states]));
    },
  };
}
