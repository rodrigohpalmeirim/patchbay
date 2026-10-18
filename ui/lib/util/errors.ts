import type { CommandError, ErrorKind } from "../types";

const KINDS: ReadonlySet<string> = new Set<ErrorKind>([
  "unknownHost",
  "readOnlyHost",
  "hostKeyRejected",
  "authFailed",
  "userRequired",
  "cancelled",
  "notConnected",
  "timeout",
  "io",
  "notFound",
  "permissionDenied",
  "alreadyExists",
  "unsupported",
  "sftp",
  "ssh",
  "other",
]);

/** Normalizes anything thrown by a backend call into a CommandError. */
export function toCommandError(e: unknown): CommandError {
  if (e && typeof e === "object" && "kind" in e && "message" in e) {
    const { kind, message } = e as { kind: unknown; message: unknown };
    if (typeof kind === "string" && KINDS.has(kind)) {
      return { kind: kind as ErrorKind, message: String(message) };
    }
  }
  if (e instanceof Error) return { kind: "other", message: e.message };
  return { kind: "other", message: typeof e === "string" ? e : "Something went wrong" };
}

export function isCancelled(e: CommandError): boolean {
  return e.kind === "cancelled";
}

/** Short human title for an error kind. */
export function errorTitle(kind: ErrorKind): string {
  switch (kind) {
    case "authFailed":
      return "Authentication failed";
    case "userRequired":
      return "Who should Patchbay log in as?";
    case "hostKeyRejected":
      return "Host key not accepted";
    case "timeout":
      return "Connection timed out";
    case "unknownHost":
      return "Unknown host";
    case "readOnlyHost":
      return "Host is read-only";
    case "notConnected":
      return "Not connected";
    case "io":
      return "Network or file error";
    case "notFound":
      return "Not found";
    case "permissionDenied":
      return "Permission denied";
    case "alreadyExists":
      return "Already exists";
    case "unsupported":
      return "Not supported";
    case "sftp":
      return "SFTP error";
    case "ssh":
      return "SSH error";
    case "cancelled":
      return "Cancelled";
    default:
      return "Something went wrong";
  }
}

/** Capitalizes the first letter of a backend message for display. */
export function sentence(message: string): string {
  const m = message.trim();
  return m ? m[0].toUpperCase() + m.slice(1) : m;
}
