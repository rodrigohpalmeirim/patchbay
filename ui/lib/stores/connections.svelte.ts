import type { CommandError, ConnectionState, HostId, RemoteCapabilities } from "../types";
import { sentence, toCommandError } from "../util/errors";
import { firstUrl } from "../util/format";
import { openInBrowser } from "../util/links";
import { call } from "./backend";
import { hosts } from "./hosts.svelte";
import { prompts } from "./prompts.svelte";
import { tabs } from "./tabs.svelte";
import { toasts } from "./toasts.svelte";
import { ui } from "./ui.svelte";

export type Phase =
  | "idle"
  | "connecting"
  | "authenticating"
  | "connected"
  | "reconnecting"
  | "disconnecting"
  | "error";

/** Coarse status used for dots and badges. */
export type Status = "off" | "busy" | "on" | "error";

export interface Conn {
  phase: Phase;
  /** Furthest stage reached in the current/last attempt (kept on error). */
  stage: "connecting" | "authenticating" | "connected" | null;
  /** Hop being connected through while `stage === "connecting"`, null = final host. */
  hop: string | null;
  /** Hops traversed in this attempt, first hop first. */
  hopsSeen: string[];
  capabilities: RemoteCapabilities | null;
  error: CommandError | null;
  /** Why an established connection was closed by the remote/network. */
  lostReason: string | null;
  attempt: number;
  connectedAt: number | null;
  /** Message the server showed while logging in (e.g. a link to approve the login). */
  banner: string | null;
}

const IDLE: Conn = {
  phase: "idle",
  stage: null,
  hop: null,
  hopsSeen: [],
  capabilities: null,
  error: null,
  lostReason: null,
  attempt: 0,
  connectedAt: null,
  banner: null,
};

export function statusOf(phase: Phase): Status {
  switch (phase) {
    case "connected":
      return "on";
    case "error":
      return "error";
    case "idle":
      return "off";
    default:
      return "busy";
  }
}

export const STATUS_LABEL: Record<Status, string> = {
  off: "Disconnected",
  busy: "Connecting",
  on: "Connected",
  error: "Connection failed",
};

class Connections {
  map = $state<Record<HostId, Conn>>({});
  /** Increments per connect() call so stale results are ignored. */
  #seq = new Map<HostId, number>();
  /** Toasts showing a login banner with a link, by host. */
  #bannerToasts = new Map<HostId, number>();

  get(id: HostId): Conn {
    return this.map[id] ?? IDLE;
  }

  status(id: HostId): Status {
    return statusOf(this.get(id).phase);
  }

  isActive(id: HostId): boolean {
    const p = this.get(id).phase;
    return p !== "idle" && p !== "error";
  }

  #ensure(id: HostId): Conn {
    this.map[id] ??= { ...IDLE, hopsSeen: [] };
    return this.map[id];
  }

  /** `user` logs in as someone else than configured (like `ssh -l`). */
  async connect(id: HostId, { user }: { user?: string } = {}) {
    const c = this.#ensure(id);
    if (c.phase === "connected" || c.phase === "connecting" || c.phase === "authenticating") return;
    const seq = (this.#seq.get(id) ?? 0) + 1;
    this.#seq.set(id, seq);
    Object.assign(c, {
      ...IDLE,
      hopsSeen: [],
      phase: "connecting",
      stage: "connecting",
    } satisfies Conn);
    try {
      await call((b) => b.connect(id, user), { toast: false });
    } catch (e) {
      if (this.#seq.get(id) !== seq) return;
      const error = toCommandError(e);
      const cur = this.#ensure(id);
      const host = hosts.get(id);
      if (error.kind === "cancelled") {
        Object.assign(cur, { ...IDLE, hopsSeen: [] });
      } else if (error.kind === "userRequired") {
        // Tailscale device nobody has logged in to yet: ask whom to log in as.
        Object.assign(cur, { ...IDLE, hopsSeen: [] });
        ui.connectAs = { hostId: id, reason: "required" };
      } else {
        cur.phase = "error";
        cur.error = error;
        cur.capabilities = null;
        // Overview tab shows errors inline; toast only when it's not visible.
        if (error.kind === "authFailed" && host?.source === "tailscale") {
          // Maybe the wrong user: offer to try another one.
          ui.connectAs = {
            hostId: id,
            reason: "failed",
            error: sentence(error.message),
            user: user ?? host.user ?? undefined,
          };
        } else if (!tabs.isHostVisible(id)) {
          toasts.error(error, `Couldn't connect to ${host?.name ?? id}`);
        }
      }
      prompts.dropForHost(id);
    }
  }

  /** Shows a login banner. Links in it (e.g. Tailscale SSH asking to approve the login) get a toast. */
  banner(id: HostId, message: string) {
    const c = this.#ensure(id);
    c.banner = c.banner ? `${c.banner}\n${message}` : message;
    const url = firstUrl(message);
    if (!url) return;
    this.#dismissBanner(id);
    const name = hosts.get(id)?.name ?? id;
    const toast = toasts.show("info", `${name} needs your approval`, message, {
      sticky: true,
      action: { label: "Open link", run: () => void openInBrowser(url) },
    });
    this.#bannerToasts.set(id, toast);
  }

  #dismissBanner(id: HostId) {
    const toast = this.#bannerToasts.get(id);
    if (toast !== undefined) toasts.dismiss(toast);
    this.#bannerToasts.delete(id);
  }

  async disconnect(id: HostId) {
    const c = this.#ensure(id);
    this.#seq.set(id, (this.#seq.get(id) ?? 0) + 1);
    const wasConnected = c.phase === "connected";
    if (wasConnected) c.phase = "disconnecting";
    try {
      await call((b) => b.disconnect(id), { action: "Couldn't disconnect" });
    } finally {
      Object.assign(this.#ensure(id), { ...IDLE, hopsSeen: [] });
      prompts.dropForHost(id);
    }
  }

  /** Clears an error / lost-connection notice. */
  dismiss(id: HostId) {
    const c = this.map[id];
    if (c && (c.phase === "error" || c.phase === "idle")) Object.assign(c, { ...IDLE, hopsSeen: [] });
  }

  forget(id: HostId) {
    delete this.map[id];
    this.#seq.delete(id);
  }

  /** Applies a `connection` core event. */
  handle(id: HostId, ev: ConnectionState) {
    const c = this.#ensure(id);
    switch (ev.state) {
      case "connecting":
        if (c.phase === "idle" || c.phase === "error" || c.phase === "connected") {
          // Started by the backend (e.g. session restore): fresh attempt.
          Object.assign(c, { ...IDLE, hopsSeen: [] });
        }
        c.phase = "connecting";
        c.stage = "connecting";
        c.hop = ev.hop;
        if (ev.hop && !c.hopsSeen.includes(ev.hop)) c.hopsSeen.push(ev.hop);
        break;
      case "authenticating":
        c.phase = "authenticating";
        c.stage = "authenticating";
        break;
      case "connected":
        this.#dismissBanner(id);
        c.banner = null;
        c.phase = "connected";
        c.stage = "connected";
        c.hop = null;
        c.capabilities = ev.capabilities;
        c.error = null;
        c.lostReason = null;
        c.connectedAt = Date.now();
        break;
      case "reconnecting":
        c.phase = "reconnecting";
        c.attempt = ev.attempt;
        break;
      case "disconnected": {
        prompts.dropForHost(id);
        this.#dismissBanner(id);
        if (c.phase === "error" || c.phase === "disconnecting") break;
        if (c.phase === "connecting" || c.phase === "authenticating") {
          if (ev.reason) {
            // Attempt failed. The connect() rejection (if any) refines this.
            c.phase = "error";
            c.error = { kind: "other", message: ev.reason };
          } else {
            Object.assign(c, { ...IDLE, hopsSeen: [] });
          }
          break;
        }
        const wasConnected = c.phase === "connected" || c.phase === "reconnecting";
        Object.assign(c, { ...IDLE, hopsSeen: [] });
        if (wasConnected && ev.reason) {
          c.lostReason = ev.reason;
          // Forwards reconnect automatically right after a drop; only tell
          // the user if nothing picks the connection up again.
          const reason = ev.reason;
          setTimeout(() => {
            if (this.get(id).phase === "idle" && !tabs.isHostVisible(id)) {
              toasts.show("info", `Disconnected from ${hosts.get(id)?.name ?? id}`, reason);
            }
          }, 1500);
        }
        break;
      }
    }
  }
}

export const connections = new Connections();
