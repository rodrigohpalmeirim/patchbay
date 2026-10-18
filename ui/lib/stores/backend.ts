import { getBackend, type Backend } from "../api";
import type { CommandError } from "../types";
import { toCommandError } from "../util/errors";
import { toasts } from "./toasts.svelte";

export interface CallOptions {
  /** Toast title shown on failure; set `toast: false` to handle errors yourself. */
  action?: string;
  toast?: boolean;
}

/**
 * Runs a backend call. Errors are normalized to CommandError, toasted (unless
 * cancelled or `toast: false`) and re-thrown.
 */
export async function call<T>(fn: (b: Backend) => Promise<T>, opts: CallOptions = {}): Promise<T> {
  try {
    return await fn(await getBackend());
  } catch (e) {
    const error: CommandError = toCommandError(e);
    if (opts.toast !== false) toasts.error(error, opts.action);
    throw error;
  }
}

/** Like `call` but never rejects; resolves to undefined on failure. */
export async function attempt<T>(
  fn: (b: Backend) => Promise<T>,
  opts: CallOptions = {},
): Promise<T | undefined> {
  try {
    return await call(fn, opts);
  } catch {
    return undefined;
  }
}
