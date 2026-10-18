import type { Host, RemoteOs } from "../types";

/** `user@hostname[:port]`, omitting the default port. */
export function address(host: Pick<Host, "user" | "hostname" | "port">): string {
  const base = host.user ? `${host.user}@${host.hostname}` : host.hostname;
  return host.port && host.port !== 22 ? `${base}:${host.port}` : base;
}

/**
 * An equivalent `ssh` command line, for copying. `shared`: ssh sees the hosts
 * saved in Patchbay (~/.ssh/config includes patchbay.conf).
 */
export function sshCommand(host: Host, shared = false): string {
  if (host.source === "sshConfig" || (host.source === "app" && shared)) return `ssh ${host.name}`;
  // ssh applies ~/.ssh/config to the MagicDNS name itself.
  if (host.source === "tailscale") return `ssh ${host.user ? `${host.user}@` : ""}${host.hostname}`;
  const parts = ["ssh"];
  if (host.port !== 22) parts.push("-p", String(host.port));
  for (const f of host.identityFiles) parts.push("-i", quote(f));
  if (host.proxyJump) parts.push("-J", quote(host.proxyJump));
  parts.push(host.user ? `${host.user}@${host.hostname}` : host.hostname);
  return parts.join(" ");
}

function quote(s: string): string {
  return /^[\w@%+=:,./~-]+$/.test(s) ? s : `'${s.replace(/'/g, `'\\''`)}'`;
}

/** RemoteOs for Tailscale's OS names ("linux", "macOS", "windows", ...). */
export function tailscaleOs(os: string): RemoteOs {
  switch (os.toLowerCase()) {
    case "linux":
      return "linux";
    case "macos":
      return "macOs";
    case "windows":
      return "windows";
    case "freebsd":
    case "openbsd":
      return "bsd";
    default:
      return "unknown";
  }
}

/** The first http(s) link in `text`. */
export function firstUrl(text: string): string | null {
  return text.match(/https?:\/\/[^\s<>"']+/)?.[0] ?? null;
}

export function osLabel(os: RemoteOs): string {
  switch (os) {
    case "linux":
      return "Linux";
    case "macOs":
      return "macOS";
    case "bsd":
      return "BSD";
    case "otherUnix":
      return "Unix";
    case "windows":
      return "Windows";
    default:
      return "Unknown";
  }
}

/** Splits a ProxyJump spec into its hops. */
export function jumpHops(spec: string | null): string[] {
  if (!spec) return [];
  return spec
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
}

/** Hop label for display: strips `user@` and default `:22`. */
export function hopLabel(hop: string): string {
  return hop.replace(/^[^@]*@/, "").replace(/:22$/, "");
}

/** Natural, case-insensitive compare for host names. */
export const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

let home: string | null = null;

/** Records the local home directory so paths can be shown as `~/…`. */
export function setHomeDir(dir: string | null) {
  home = dir ? dir.replace(/[\\/]+$/, "") : null;
}

/** Abbreviates the local home directory to `~`. */
export function tildify(path: string): string {
  if (!home) return path;
  if (path === home) return "~";
  for (const sep of ["/", "\\"]) {
    if (path.startsWith(home + sep)) return "~" + sep + path.slice(home.length + 1);
  }
  return path;
}

// ------------------------------------------------------------------- files

const UNITS = ["B", "KB", "MB", "GB", "TB", "PB"];

/** Human file size with 1024 steps: "0 B", "812 B", "4.2 MB", "12 GB". */
export function formatBytes(n: number | null | undefined): string {
  if (n == null || !Number.isFinite(n)) return "—";
  if (n < 1024) return `${n} B`;
  let v = n;
  let u = 0;
  while (v >= 1024 && u < UNITS.length - 1) {
    v /= 1024;
    u++;
  }
  return `${v >= 100 ? Math.round(v) : v >= 10 ? v.toFixed(1).replace(/\.0$/, "") : v.toFixed(1)} ${UNITS[u]}`;
}

export function formatSpeed(bytesPerSec: number): string {
  return `${formatBytes(Math.round(bytesPerSec))}/s`;
}

/** Compact duration for ETAs: "8s", "3m 20s", "1h 4m". */
export function formatDuration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "—";
  const s = Math.max(1, Math.round(seconds));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${String(s % 60).padStart(2, "0")}s`;
  const h = Math.floor(m / 60);
  if (h < 48) return `${h}h ${m % 60}m`;
  return `${Math.floor(h / 24)}d ${h % 24}h`;
}

const timeFmt = new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit" });
const dayFmt = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" });
const dateFmt = new Intl.DateTimeFormat(undefined, { year: "numeric", month: "short", day: "numeric" });
const fullFmt = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "medium" });

/** Relative-ish timestamp for file lists. `secs` = unix seconds. */
export function formatModified(secs: number | null, now = Date.now()): string {
  if (secs == null) return "—";
  const d = new Date(secs * 1000);
  const diff = (now - d.getTime()) / 1000;
  if (diff >= 0 && diff < 60) return "Just now";
  if (diff >= 0 && diff < 3600) return `${Math.floor(diff / 60)} min ago`;
  const today = new Date(now);
  today.setHours(0, 0, 0, 0);
  const t = d.getTime();
  if (t >= today.getTime() && t < today.getTime() + 86_400_000) return `Today, ${timeFmt.format(d)}`;
  if (t >= today.getTime() - 86_400_000 && t < today.getTime()) return `Yesterday, ${timeFmt.format(d)}`;
  if (d.getFullYear() === today.getFullYear()) return `${dayFmt.format(d)}, ${timeFmt.format(d)}`;
  return dateFmt.format(d);
}

export function formatExact(secs: number | null): string {
  return secs == null ? "Unknown" : fullFmt.format(new Date(secs * 1000));
}

/** `ls -l` style mode string, e.g. "drwxr-xr-x" (with s/S/t/T for special bits). */
export function permString(mode: number | null, kind: "file" | "dir" | "symlink" | "other" = "file"): string {
  if (mode == null) return "—";
  const type = kind === "dir" ? "d" : kind === "symlink" ? "l" : kind === "other" ? "?" : "-";
  const bit = (b: number, ch: string) => (mode & b ? ch : "-");
  const special = (exec: number, sp: number, on: string, off: string, plain: string) =>
    mode & sp ? (mode & exec ? on : off) : mode & exec ? plain : "-";
  return (
    type +
    bit(0o400, "r") +
    bit(0o200, "w") +
    special(0o100, 0o4000, "s", "S", "x") +
    bit(0o040, "r") +
    bit(0o020, "w") +
    special(0o010, 0o2000, "s", "S", "x") +
    bit(0o004, "r") +
    bit(0o002, "w") +
    special(0o001, 0o1000, "t", "T", "x")
  );
}

export function octal(mode: number | null): string {
  if (mode == null) return "";
  const s = (mode & 0o7777).toString(8);
  return s.length < 3 ? s.padStart(3, "0") : s.length === 3 ? s : s.padStart(4, "0");
}

/** "1 item" / "3 items". */
export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n.toLocaleString()} ${n === 1 ? one : many}`;
}
