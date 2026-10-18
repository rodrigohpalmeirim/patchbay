import type { HostId, PromptPayload, PromptResponse } from "../types";
import { attempt } from "./backend";

/**
 * Edit conflicts are about a file, not a connection attempt: they don't make
 * a host "wait for input" and survive its disconnection.
 */
const aboutConnection = (p: PromptPayload) => p.prompt.kind !== "editConflict";

/** Pending prompts (auth, host keys, conflicts), answered one at a time. */
class Prompts {
  queue = $state<PromptPayload[]>([]);
  current = $derived<PromptPayload | null>(this.queue[0] ?? null);

  push(p: PromptPayload) {
    if (this.queue.some((q) => q.id === p.id)) return;
    this.queue = [...this.queue, p];
  }

  isWaiting(hostId: HostId): boolean {
    return this.queue.some((q) => q.hostId === hostId && aboutConnection(q));
  }

  async respond(id: string, response: PromptResponse) {
    this.queue = this.queue.filter((q) => q.id !== id);
    await attempt((b) => b.respondPrompt(id, response), { action: "Couldn't send response" });
  }

  /** The backend gave up waiting for this prompt. */
  close(id: string) {
    this.queue = this.queue.filter((q) => q.id !== id);
  }

  /** Forget prompts of a host whose connection attempt has ended. */
  dropForHost(hostId: HostId) {
    if (this.isWaiting(hostId)) this.queue = this.queue.filter((q) => q.hostId !== hostId || !aboutConnection(q));
  }
}

export const prompts = new Prompts();
