// Path helpers that work for both path flavours the UI sees:
//   - remote paths: always POSIX-style, incl. Windows OpenSSH (`/C:/Users/x`)
//   - local paths: native, so `C:\Users\x` (or UNC `\\server\share`) on Windows
// Nothing here touches the filesystem.

export type PathStyle = "posix" | "windows";

/** Native Windows path (drive letter with backslash, or UNC)? */
export function styleOf(path: string): PathStyle {
  return /^[A-Za-z]:(\\|$)/.test(path) || path.startsWith("\\\\") ? "windows" : "posix";
}

export function sepOf(path: string): "/" | "\\" {
  return styleOf(path) === "windows" ? "\\" : "/";
}

/** Root of a path: "/", "C:\" or "\\server\share\". */
function rootOf(path: string): string {
  if (styleOf(path) === "posix") return "/";
  const unc = /^\\\\[^\\]+\\[^\\]+\\?/.exec(path);
  if (unc) return unc[0].endsWith("\\") ? unc[0] : unc[0] + "\\";
  return path.slice(0, 2).toUpperCase() + "\\";
}

export function isRoot(path: string): boolean {
  return normalize(path) === rootOf(path);
}

/** Collapses duplicate separators, resolves `.`/`..`, drops trailing separators. */
export function normalize(path: string): string {
  if (!path) return path;
  const style = styleOf(path);
  const sep = style === "windows" ? "\\" : "/";
  const root = rootOf(path);
  const rest = style === "windows" ? path.slice(root.length - (root.endsWith("\\") ? 1 : 0)) : path;
  const parts: string[] = [];
  for (const part of rest.split(style === "windows" ? /[\\/]+/ : /\/+/)) {
    if (!part || part === ".") continue;
    if (part === "..") parts.pop();
    else parts.push(part);
  }
  if (style === "posix" && !path.startsWith("/")) return parts.join("/") || ".";
  return root + parts.join(sep);
}

export function join(dir: string, name: string): string {
  const sep = sepOf(dir);
  return dir.endsWith(sep) || dir.endsWith("/") ? dir + name : dir + sep + name;
}

/** Parent directory, or null at the root. */
export function dirname(path: string): string | null {
  const p = normalize(path);
  if (p === rootOf(p)) return null;
  const sep = sepOf(p);
  const i = p.lastIndexOf(sep);
  if (i === -1) return null;
  const parent = p.slice(0, i);
  const root = rootOf(p);
  return parent.length < root.length ? root : parent || root;
}

export function basename(path: string): string {
  const p = normalize(path);
  if (p === rootOf(p)) return p;
  const sep = sepOf(p);
  return p.slice(p.lastIndexOf(sep) + 1);
}

/** Lower-cased extension without the dot ("" if none; dotfiles have none). */
export function extension(name: string): string {
  const i = name.lastIndexOf(".");
  return i > 0 ? name.slice(i + 1).toLowerCase() : "";
}

export interface Crumb {
  name: string;
  path: string;
}

/** Breadcrumb segments from the root down to `path`. */
export function crumbs(path: string): Crumb[] {
  const p = normalize(path);
  const root = rootOf(p);
  const style = styleOf(p);
  const out: Crumb[] = [{ name: style === "windows" ? root.replace(/\\$/, "") : "/", path: root }];
  const sep = style === "windows" ? "\\" : "/";
  let acc = root;
  for (const part of p.slice(root.length).split(sep).filter(Boolean)) {
    acc = acc.endsWith(sep) ? acc + part : acc + sep + part;
    out.push({ name: part, path: acc });
  }
  return out;
}

/** True if `child` is `parent` or inside it. */
export function isWithin(parent: string, child: string): boolean {
  const a = normalize(parent);
  const b = normalize(child);
  if (a === b) return true;
  const sep = sepOf(a);
  return b.startsWith(a.endsWith(sep) ? a : a + sep);
}

/**
 * Resolves what the user typed in the path bar: absolute paths, `~`, and
 * paths relative to the current directory.
 */
export function resolveInput(input: string, current: string, home: string | null): string {
  let s = input.trim();
  if (!s) return current;
  if (home && (s === "~" || s.startsWith("~/") || s.startsWith("~\\"))) s = home + s.slice(1);
  const absolute =
    s.startsWith("/") || styleOf(s) === "windows" || (styleOf(current) === "windows" && s.startsWith("\\"));
  if (!absolute) s = join(current, s);
  else if (styleOf(current) === "windows" && s.startsWith("\\") && !s.startsWith("\\\\")) s = rootOf(current) + s.slice(1);
  return normalize(s);
}

/** Path of `child` relative to `root` (for search results). */
export function relative(root: string, child: string): string {
  const a = normalize(root);
  const b = normalize(child);
  if (a === b) return "";
  const sep = sepOf(a);
  const prefix = a.endsWith(sep) ? a : a + sep;
  return b.startsWith(prefix) ? b.slice(prefix.length) : b;
}
