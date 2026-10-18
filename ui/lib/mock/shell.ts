// Mock implementations of the terminal* backend commands: a small fake shell
// per terminal, running on the same in-memory filesystem as the files tabs
// (so `ls` matches what the browser shows).
//
// Supports a readline-ish line editor (cursor movement, history, Ctrl+A/E/U/K/W,
// Alt+B/F, Ctrl+R reverse search, Tab completion, Ctrl+L, Ctrl+C, Ctrl+D),
// bracketed paste, OSC 0/7 (title + cwd) at every prompt, and a handful of
// commands -- type `help`. Perf testing: `yes`, `flood [seconds]`, `seq N`,
// `colors` prints the 16/256/truecolor palettes.
//
// `drop-connection` simulates a network failure for a remote host (all its
// terminals end with a reason, the connection reports "lost"). Shells on
// legacy-nas (FreeBSD sh) and win-buildbox (PowerShell) don't enable
// bracketed paste, so pasting several lines there asks for confirmation;
// `bracketed-paste on|off` toggles it anywhere.

import type { Backend } from "../api";
import type { CommandError, CoreEvent, Host, HostId, RemoteCapabilities, TerminalInfo } from "../types";
import { readContent } from "./content";
import { kids, normalize, resolve, tryResolve, type DirNode, type Machine, type Node } from "./vfs";

type TerminalMethods = Pick<
  Backend,
  "terminalOpen" | "terminalWrite" | "terminalResize" | "terminalClose" | "terminalAck"
>;

export interface ShellDeps {
  host(id: HostId): Host | undefined;
  isConnected(id: HostId): boolean;
  capabilities(id: HostId): RemoteCapabilities | null;
  machine(hostId: HostId | null): Machine;
  emit(e: CoreEvent): void;
  /** Simulates the network dropping the connection to `id`. */
  dropConnection(id: HostId, reason: string): void;
}

function err(kind: CommandError["kind"], message: string): CommandError {
  return { kind, message };
}

const enc = new TextEncoder();
const dec = new TextDecoder();
const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

// SGR helpers
const sgr = (codes: string, s: string) => `\x1b[${codes}m${s}\x1b[0m`;
const bold = (s: string) => sgr("1", s);
const dim = (s: string) => sgr("2", s);

const ARCHIVE = /\.(tar|tgz|gz|zst|xz|bz2|zip|7z|deb|rpm|msi|dmg|iso|pkg)$/i;
const IMAGE = /\.(png|jpe?g|gif|webp|svg|bmp|ico|tiff?)$/i;
const MEDIA = /\.(mp4|mkv|mov|avi|webm|mp3|flac|wav|ogg|m4a)$/i;

interface Job {
  cancel(): void;
}

/** Visible width of a string without escape sequences (BMP only; good enough). */
function visibleLength(s: string): number {
  return s.replace(/\x1b\[[0-9;:?]*[A-Za-z]/g, "").replace(/\x1b\][^\x07\x1b]*(\x07|\x1b\\)/g, "").length;
}

function permString(n: Node): string {
  const t = n.kind === "dir" ? "d" : n.kind === "symlink" ? "l" : "-";
  const p = n.kind === "symlink" ? 0o777 : n.permissions;
  const rwx = (v: number) => `${v & 4 ? "r" : "-"}${v & 2 ? "w" : "-"}${v & 1 ? "x" : "-"}`;
  let s = t + rwx((p >> 6) & 7) + rwx((p >> 3) & 7) + rwx(p & 7);
  if (p & 0o4000) s = s.slice(0, 3) + (p & 0o100 ? "s" : "S") + s.slice(4);
  if (p & 0o1000) s = s.slice(0, 9) + (p & 1 ? "t" : "T");
  return s;
}

function humanSize(n: number): string {
  if (n < 1024) return `${n}`;
  const units = ["K", "M", "G", "T"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)}${units[i]}`;
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
function lsDate(secs: number): string {
  const d = new Date(secs * 1000);
  const recent = Date.now() - d.getTime() < 180 * 86400 * 1000;
  const day = String(d.getDate()).padStart(2, " ");
  const tail = recent ? `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}` : ` ${d.getFullYear()}`;
  return `${MONTHS[d.getMonth()]} ${day} ${tail}`;
}

/** Splits a command line into commands separated by `;` / `&&`, then words. */
function parse(line: string): { words: string[]; and: boolean }[] {
  const out: { words: string[]; and: boolean }[] = [];
  let words: string[] = [];
  let cur = "";
  let has = false;
  let quote: string | null = null;
  let and = false;
  const pushWord = () => {
    if (has) words.push(cur);
    cur = "";
    has = false;
  };
  const pushCmd = (nextAnd: boolean) => {
    pushWord();
    if (words.length) out.push({ words, and });
    words = [];
    and = nextAnd;
  };
  for (let i = 0; i < line.length; i++) {
    const c = line[i];
    if (quote) {
      if (c === quote) quote = null;
      else cur += c;
      continue;
    }
    if (c === "'" || c === '"') {
      quote = c;
      has = true;
    } else if (c === " " || c === "\t" || c === "\n") pushWord();
    else if (c === ";") pushCmd(false);
    else if (c === "&" && line[i + 1] === "&") {
      i++;
      pushCmd(true);
    } else {
      cur += c;
      has = true;
    }
  }
  pushCmd(false);
  return out;
}

const HELP = `Patchbay mock shell -- a stand-in for a real shell while developing the UI.

  ls [-la] [path]   cd [path]   pwd   cat FILE   head/tail [-n N] FILE
  echo ...          clear       whoami   hostname   uname [-a]   date
  uptime   df -h    free -h     history  env        sudo CMD   systemctl
  colors            16 / 256 / truecolor palette and text styles
  yes [text]        endless output (Ctrl+C stops)
  flood [seconds]   colored high-volume log output (default 10 s)
  seq N             numbers 1..N          sleep N     wait N seconds
  title TEXT        set the window title  exit [code]
  bracketed-paste on|off                  drop-connection (remote only)

Line editing: arrows, Home/End, Ctrl+A/E/U/K/W, Alt+B/F, Ctrl+R, Tab, Ctrl+L.
`;

class Shell {
  readonly m: Machine;
  cwd: string;
  oldPwd: string | null = null;
  readonly user: string;
  readonly hostname: string;
  readonly win: boolean;
  readonly bsd: boolean;
  readonly remote: boolean;

  cols: number;
  rows: number;
  bracketed: boolean;
  closed = false;
  title: string | null = null;

  // line editor
  line = "";
  pos = 0;
  /** Screen rows (above the cursor row) occupied by the prompt + line. */
  cursorRow = 0;
  history: string[] = [];
  hIndex = -1;
  draft = "";
  search: { query: string; match: string | null } | null = null;
  inPaste = false;

  queue = "";
  job: Job | null = null;
  lastExit = 0;

  #pending = "";
  #flushTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(
    readonly id: string,
    readonly hostId: HostId | null,
    private deps: ShellDeps,
    private onData: (d: Uint8Array) => void,
    opts: { cols: number; rows: number; cwd: string | null; os: RemoteCapabilities["os"] },
  ) {
    this.m = deps.machine(hostId);
    this.cols = Math.max(10, opts.cols);
    this.rows = Math.max(4, opts.rows);
    this.user = this.m.user;
    this.win = this.m.windows;
    this.bsd = opts.os === "bsd";
    this.remote = hostId !== null;
    this.hostname = hostId ? (this.m.label || "remote") : "laptop";
    this.bracketed = !this.win && !this.bsd;
    this.cwd = this.m.home;
    if (opts.cwd) {
      const p = normalize(opts.cwd);
      const r = tryResolve(this.m, p, true);
      if (r?.node.kind === "dir" && !r.node.denied) this.cwd = r.real === p ? p : r.real;
      else this.print(`${this.win ? "Set-Location" : "cd"}: ${opts.cwd}: No such directory -- starting in home\n`);
    }
  }

  // ------------------------------------------------------------- output

  raw(s: string) {
    if (this.closed) return;
    this.#pending += s;
    if (this.#pending.length > 64 * 1024) this.flush();
    else this.#flushTimer ??= setTimeout(() => this.flush(), 0);
  }

  print(s: string) {
    this.raw(s.replace(/\r?\n/g, "\r\n"));
  }

  flush() {
    if (this.#flushTimer) clearTimeout(this.#flushTimer);
    this.#flushTimer = null;
    if (!this.#pending || this.closed) return;
    const chunk = this.#pending;
    this.#pending = "";
    this.onData(enc.encode(chunk));
  }

  // -------------------------------------------------------------- prompt

  display(path: string): string {
    if (this.win) return path.replace(/^\/([A-Za-z]:)/, "$1").replace(/\//g, "\\") || "\\";
    if (path === this.m.home) return "~";
    if (path.startsWith(this.m.home + "/")) return "~" + path.slice(this.m.home.length);
    return path;
  }

  promptText(): string {
    if (this.win) return `PS ${this.display(this.cwd)}> `;
    if (this.bsd) return `${this.user}@${this.hostname}:${this.display(this.cwd)} $ `;
    const sigil = this.user === "root" ? "#" : "$";
    return `\x1b[1;32m${this.user}@${this.hostname}\x1b[0m:\x1b[1;34m${this.display(this.cwd)}\x1b[0m${sigil} `;
  }

  prompt() {
    const title = this.title ?? (this.win ? `PS ${this.display(this.cwd)}` : `${this.user}@${this.hostname}: ${this.display(this.cwd)}`);
    let s = `\x1b]0;${title}\x07`;
    if (!this.win) s += `\x1b]7;file://${this.hostname}${encodeURI(this.cwd)}\x1b\\`;
    s += this.bracketed ? "\x1b[?2004h" : "";
    this.raw(s);
    this.line = "";
    this.pos = 0;
    this.cursorRow = 0;
    this.hIndex = -1;
    this.redraw();
  }

  /** Rows/cols of `text` (no escapes) written from column 0, with wrapping. */
  #measure(text: string): { row: number; col: number } {
    let row = 0;
    let col = 0;
    for (const ch of text) {
      if (ch === "\n") {
        row++;
        col = 0;
        continue;
      }
      if (col >= this.cols) {
        row++;
        col = 0;
      }
      col++;
    }
    if (col >= this.cols) {
      // xterm keeps the cursor in the last column with a pending wrap.
      col = this.cols - 1;
    }
    return { row, col };
  }

  /** Redraws prompt + edit line and places the cursor at `pos`. */
  redraw() {
    const promptRaw = this.search ? `(reverse-i-search)\`${this.search.query}': ` : this.promptText();
    const body = this.search ? (this.search.match ?? "") : this.line;
    const plain = " ".repeat(visibleLength(promptRaw)) + body.replace(/\n/g, "\n> ");
    const shown = body.replace(/\n/g, "\r\n> ");
    let s = this.cursorRow > 0 ? `\x1b[${this.cursorRow}A` : "";
    s += `\r\x1b[J${promptRaw}${shown}`;
    const end = this.#measure(plain);
    const cursorAt = this.search ? plain.length : visibleLength(promptRaw) + this.line.slice(0, this.pos).replace(/\n/g, "\n> ").length;
    const target = this.#measure(plain.slice(0, cursorAt));
    // Pending-wrap at the very end: emit a wrap so relative moves are sane.
    if (end.row > target.row) s += `\x1b[${end.row - target.row}A`;
    s += `\r${target.col > 0 ? `\x1b[${target.col}C` : ""}`;
    this.cursorRow = target.row;
    this.raw(s);
  }

  /** Moves below the edit line so output starts on a fresh row. */
  #finishLine() {
    const plain = " ".repeat(visibleLength(this.promptText())) + this.line.replace(/\n/g, "\n> ");
    const end = this.#measure(plain);
    const down = end.row - this.cursorRow;
    this.raw(`${down > 0 ? `\x1b[${down}B` : ""}\r\n`);
    this.cursorRow = 0;
  }

  // --------------------------------------------------------------- input

  write(data: string) {
    if (this.closed) return;
    this.queue += data;
    this.#drain();
  }

  #drain() {
    while (this.queue && !this.closed) {
      if (this.job) {
        // Only Ctrl+C reaches a running command; the rest is typeahead.
        const i = this.queue.indexOf("\x03");
        if (i === -1) return;
        this.queue = this.queue.slice(i + 1);
        const job = this.job;
        this.job = null;
        job.cancel();
        this.raw("^C\r\n");
        this.lastExit = 130;
        this.prompt();
        continue;
      }
      const q = this.queue;
      let n = 1;
      const esc = /^\x1b(\[[0-9;]*[~A-Za-z]|O[A-Za-z]|.)?/s.exec(q);
      if (q.startsWith("\x1b[200~")) {
        n = 6;
        this.inPaste = true;
      } else if (q.startsWith("\x1b[201~")) {
        n = 6;
        this.inPaste = false;
      } else if (q[0] === "\x1b" && esc) {
        n = esc[0].length;
        this.#escape(esc[0]);
      } else {
        this.#key(q[0]);
      }
      this.queue = this.queue.slice(n);
    }
  }

  #escape(seq: string): void {
    if (this.search) {
      this.#endSearch(false);
      if (seq === "\x1b") return;
    }
    switch (seq) {
      case "\x1b[A":
      case "\x1bOA":
        return this.#historyMove(1);
      case "\x1b[B":
      case "\x1bOB":
        return this.#historyMove(-1);
      case "\x1b[C":
      case "\x1bOC":
        if (this.pos < this.line.length) this.pos++;
        return this.redraw();
      case "\x1b[D":
      case "\x1bOD":
        if (this.pos > 0) this.pos--;
        return this.redraw();
      case "\x1b[H":
      case "\x1bOH":
      case "\x1b[1~":
        this.pos = 0;
        return this.redraw();
      case "\x1b[F":
      case "\x1bOF":
      case "\x1b[4~":
        this.pos = this.line.length;
        return this.redraw();
      case "\x1b[3~":
        if (this.pos < this.line.length) this.line = this.line.slice(0, this.pos) + this.line.slice(this.pos + 1);
        return this.redraw();
      case "\x1bb":
      case "\x1b[1;3D":
      case "\x1b[1;5D":
        this.pos = this.#wordStart(this.pos);
        return this.redraw();
      case "\x1bf":
      case "\x1b[1;3C":
      case "\x1b[1;5C":
        this.pos = this.#wordEnd(this.pos);
        return this.redraw();
      case "\x1b\x7f": {
        const s = this.#wordStart(this.pos);
        this.line = this.line.slice(0, s) + this.line.slice(this.pos);
        this.pos = s;
        return this.redraw();
      }
      default:
        return;
    }
  }

  #wordStart(p: number): number {
    while (p > 0 && /\s/.test(this.line[p - 1])) p--;
    while (p > 0 && !/\s/.test(this.line[p - 1])) p--;
    return p;
  }

  #wordEnd(p: number): number {
    const n = this.line.length;
    while (p < n && /\s/.test(this.line[p])) p++;
    while (p < n && !/\s/.test(this.line[p])) p++;
    return p;
  }

  #insert(s: string) {
    this.line = this.line.slice(0, this.pos) + s + this.line.slice(this.pos);
    this.pos += s.length;
  }

  #key(c: string): void {
    if (this.search) {
      if (c === "\x12") return this.#searchNext();
      if (c === "\x03" || c === "\x07") return this.#endSearch(false, true);
      if (c === "\r") {
        this.#endSearch(true);
        return this.#key("\r");
      }
      if (c === "\x7f" || c === "\b") {
        this.search.query = this.search.query.slice(0, -1);
        return this.#searchUpdate();
      }
      if (c >= " ") {
        this.search.query += c;
        return this.#searchUpdate();
      }
      this.#endSearch(true);
    }
    if (this.inPaste) {
      // Bracketed paste: newlines are inserted, not executed.
      this.#insert(c === "\r" || c === "\n" ? "\n" : c === "\t" ? "  " : c);
      return this.redraw();
    }
    switch (c) {
      case "\r":
        return this.#enter();
      case "\n":
        return;
      case "\x7f":
      case "\b":
        if (this.pos > 0) {
          this.line = this.line.slice(0, this.pos - 1) + this.line.slice(this.pos);
          this.pos--;
        }
        return this.redraw();
      case "\x01":
        this.pos = 0;
        return this.redraw();
      case "\x05":
        this.pos = this.line.length;
        return this.redraw();
      case "\x02":
        if (this.pos > 0) this.pos--;
        return this.redraw();
      case "\x06":
        if (this.pos < this.line.length) this.pos++;
        return this.redraw();
      case "\x15":
        this.line = this.line.slice(this.pos);
        this.pos = 0;
        return this.redraw();
      case "\x0b":
        this.line = this.line.slice(0, this.pos);
        return this.redraw();
      case "\x17": {
        const s = this.#wordStart(this.pos);
        this.line = this.line.slice(0, s) + this.line.slice(this.pos);
        this.pos = s;
        return this.redraw();
      }
      case "\x0c":
        this.raw("\x1b[H\x1b[2J");
        this.cursorRow = 0;
        return this.redraw();
      case "\x03":
        this.#finishLineWith("^C");
        this.lastExit = 130;
        return this.prompt();
      case "\x04":
        if (!this.line) {
          this.#finishLineWith(this.remote ? "logout" : "exit");
          return this.exit(0);
        }
        if (this.pos < this.line.length) this.line = this.line.slice(0, this.pos) + this.line.slice(this.pos + 1);
        return this.redraw();
      case "\x10":
        return this.#historyMove(1);
      case "\x0e":
        return this.#historyMove(-1);
      case "\x12":
        this.search = { query: "", match: null };
        return this.redraw();
      case "\t":
        return this.#complete();
      default:
        if (c >= " ") {
          this.#insert(c);
          this.redraw();
        }
    }
  }

  #finishLineWith(s: string) {
    this.pos = this.line.length;
    this.redraw();
    this.raw(s);
    this.#finishLine();
  }

  #historyMove(dir: 1 | -1) {
    if (!this.history.length) return;
    if (this.hIndex === -1) {
      if (dir === -1) return;
      this.draft = this.line;
    }
    const next = this.hIndex + dir;
    if (next >= this.history.length) return;
    this.hIndex = next;
    this.line = next === -1 ? this.draft : this.history[this.history.length - 1 - next];
    this.pos = this.line.length;
    this.redraw();
  }

  #searchUpdate() {
    if (!this.search) return;
    const q = this.search.query;
    this.search.match = q ? ([...this.history].reverse().find((h) => h.includes(q)) ?? this.search.match) : null;
    this.redraw();
  }

  #searchNext() {
    if (!this.search) return;
    const { query, match } = this.search;
    const rev = [...this.history].reverse();
    const start = match ? rev.indexOf(match) + 1 : 0;
    const next = rev.slice(start).find((h) => h.includes(query));
    if (next) this.search.match = next;
    this.redraw();
  }

  #endSearch(accept: boolean, cancel = false) {
    const s = this.search;
    if (!s) return;
    this.search = null;
    if (accept && s.match) {
      this.line = s.match;
      this.pos = this.line.length;
    }
    if (cancel) {
      this.line = "";
      this.pos = 0;
    }
    this.redraw();
  }

  #complete() {
    const before = this.line.slice(0, this.pos);
    const wordStart = Math.max(before.lastIndexOf(" ") + 1, 0);
    const word = before.slice(wordStart);
    const slash = word.lastIndexOf("/");
    const dirPart = slash === -1 ? "" : word.slice(0, slash + 1);
    const prefix = word.slice(slash + 1);
    const isFirst = wordStart === 0;
    let names: { name: string; dir: boolean }[] = [];
    if (isFirst && !dirPart) {
      names = COMMANDS.filter((c) => c.startsWith(prefix)).map((name) => ({ name, dir: false }));
    } else {
      try {
        const dir = this.#dir(this.#abs(dirPart || "."));
        names = [...kids(dir).values()]
          .filter((n) => n.name.startsWith(prefix) && (prefix.startsWith(".") || !n.name.startsWith(".")))
          .map((n) => ({ name: n.name, dir: n.kind === "dir" || (n.kind === "symlink" && tryResolve(this.m, this.#abs(dirPart + n.name), true)?.node.kind === "dir") }));
      } catch {
        names = [];
      }
    }
    if (!names.length) return this.raw("\x07");
    if (names.length === 1) {
      const n = names[0];
      const quoted = /\s/.test(n.name) ? n.name.replace(/ /g, "\\ ") : n.name;
      this.#insert(quoted.slice(prefix.length) + (n.dir ? "/" : " "));
      return this.redraw();
    }
    // Longest common prefix, then list candidates.
    let common = names[0].name;
    for (const n of names) while (!n.name.startsWith(common)) common = common.slice(0, -1);
    if (common.length > prefix.length) {
      this.#insert(common.slice(prefix.length));
      return this.redraw();
    }
    this.#finishLineKeep();
    this.#columns(names.map((n) => ({ text: n.name + (n.dir ? "/" : ""), color: n.dir ? "1;34" : "" })));
    this.cursorRow = 0;
    this.redraw();
  }

  /** Moves to a new row below the edit line without clearing it. */
  #finishLineKeep() {
    const save = this.pos;
    this.pos = this.line.length;
    this.redraw();
    this.#finishLine();
    this.pos = save;
  }

  #enter() {
    const line = this.line;
    this.pos = line.length;
    this.redraw();
    this.#finishLine();
    this.raw(this.bracketed ? "\x1b[?2004l" : "");
    if (line.trim()) {
      if (this.history[this.history.length - 1] !== line) this.history.push(line);
    }
    this.line = "";
    this.pos = 0;
    void this.#runLine(line);
  }

  // ------------------------------------------------------------ commands

  async #runLine(line: string) {
    const cmds = parse(line.replace(/\\\n/g, ""));
    const running = { cancelled: false };
    const job: Job = { cancel: () => (running.cancelled = true) };
    this.job = job;
    for (const { words, and } of cmds) {
      if (running.cancelled || this.closed) break;
      if (and && this.lastExit !== 0) continue;
      try {
        this.lastExit = (await this.#run(words.map((w) => this.#expand(w)), running)) ?? 0;
      } catch (e) {
        this.print(`${(e as Error).message ?? String(e)}\n`);
        this.lastExit = 1;
      }
      if (this.lastExit === -1) return; // exited
    }
    if (this.closed) return;
    if (this.job === job) this.job = null;
    if (!running.cancelled) this.prompt();
    this.#drain();
  }

  #expand(w: string): string {
    if (w === "~" || w.startsWith("~/")) w = this.m.home + w.slice(1);
    return w.replace(/\$(\?|[A-Za-z_][A-Za-z0-9_]*)/g, (_, name: string) => this.#env()[name] ?? "");
  }

  #env(): Record<string, string> {
    return {
      HOME: this.m.home,
      USER: this.user,
      LOGNAME: this.user,
      PWD: this.cwd,
      SHELL: this.win ? "powershell.exe" : this.bsd ? "/bin/sh" : "/bin/bash",
      HOSTNAME: this.hostname,
      TERM: "xterm-256color",
      COLORTERM: "truecolor",
      LANG: "en_US.UTF-8",
      COLUMNS: String(this.cols),
      LINES: String(this.rows),
      "?": String(this.lastExit),
    };
  }

  #abs(p: string): string {
    if (this.win && /^[A-Za-z]:/.test(p)) return normalize("/" + p);
    return normalize(p.startsWith("/") ? p : `${this.cwd}/${p}`);
  }

  #dir(path: string): DirNode {
    const r = resolve(this.m, path, true);
    if (r.node.kind !== "dir") throw new Error(`not a directory: ${path}`);
    if (r.node.denied) throw new Error(`permission denied: ${path}`);
    return r.node;
  }

  #columns(items: { text: string; color: string }[]) {
    if (!items.length) return;
    const width = Math.max(...items.map((i) => i.text.length)) + 2;
    const perRow = Math.max(1, Math.floor(this.cols / width));
    const rows = Math.ceil(items.length / perRow);
    let out = "";
    for (let r = 0; r < rows; r++) {
      for (let c = 0; c < perRow; c++) {
        const it = items[c * rows + r];
        if (!it) continue;
        const last = (c + 1) * rows + r >= items.length;
        const txt = it.color ? sgr(it.color, it.text) : it.text;
        out += last ? txt : txt + " ".repeat(width - it.text.length);
      }
      out += "\n";
    }
    this.print(out);
  }

  #colorFor(n: Node, target: Node | null): string {
    if (n.kind === "symlink") return target ? "1;36" : "1;31;40";
    if (n.kind === "dir") return n.permissions & 0o002 ? "30;42" : "1;34";
    if (n.permissions & 0o4000) return "37;41";
    if (n.permissions & 0o111) return "1;32";
    if (ARCHIVE.test(n.name)) return "1;31";
    if (IMAGE.test(n.name)) return "1;35";
    if (MEDIA.test(n.name)) return "36";
    return "";
  }

  async #run(argv: string[], job: { cancelled: boolean }): Promise<number> {
    const [cmd, ...args] = argv;
    if (!cmd) return 0;
    const name = this.win ? cmd.toLowerCase() : cmd;
    switch (name) {
      case "help":
        this.print(HELP);
        return 0;
      case "ls":
      case "ll":
      case "dir":
      case "get-childitem":
        return this.#ls(name === "ll" ? ["-la", ...args] : args);
      case "cd":
      case "set-location":
        return this.#cd(args[0]);
      case "pwd":
      case "get-location":
        this.print(`${this.win ? this.display(this.cwd) : this.cwd}\n`);
        return 0;
      case "echo":
      case "write-output":
        if (args[0] === "-e" || args[0] === "-n") {
          const text = args.slice(1).join(" ").replace(/\\e|\\033|\\x1b/g, "\x1b").replace(/\\n/g, "\n").replace(/\\t/g, "\t").replace(/\\a/g, "\x07");
          this.print(args[0] === "-n" ? args.slice(1).join(" ") : text + "\n");
        } else this.print(`${args.join(" ")}\n`);
        return 0;
      case "cat":
      case "type":
      case "get-content":
        return this.#cat(args);
      case "head":
      case "tail":
        return this.#headTail(name, args);
      case "clear":
      case "cls":
      case "reset":
        this.raw("\x1b[H\x1b[2J\x1b[3J");
        return 0;
      case "whoami":
        this.print(`${this.win ? `${this.hostname}\\${this.user}` : this.user}\n`);
        return 0;
      case "hostname":
        this.print(`${this.hostname}\n`);
        return 0;
      case "uname":
        this.print(
          args.includes("-a")
            ? this.bsd
              ? `FreeBSD ${this.hostname} 14.1-RELEASE FreeBSD 14.1-RELEASE amd64\n`
              : `Linux ${this.hostname} 6.1.0-26-amd64 #1 SMP PREEMPT_DYNAMIC Debian 6.1.112-1 (2024-09-30) x86_64 GNU/Linux\n`
            : this.bsd
              ? "FreeBSD\n"
              : "Linux\n",
        );
        return 0;
      case "date":
        this.print(`${new Date().toString()}\n`);
        return 0;
      case "uptime":
        this.print(` ${new Date().toTimeString().slice(0, 8)} up 41 days,  3:12,  2 users,  load average: 0.08, 0.12, 0.10\n`);
        return 0;
      case "df":
        this.print(
          [
            "Filesystem      Size  Used Avail Use% Mounted on",
            "udev            7.8G     0  7.8G   0% /dev",
            "tmpfs           1.6G  1.2M  1.6G   1% /run",
            `/dev/nvme0n1p2  468G  211G  234G  ${sgr("33", "48%")} /`,
            "tmpfs           7.8G     0  7.8G   0% /dev/shm",
            `/dev/nvme0n1p1  511M  5.9M  506M   2% /boot/efi`,
            `/dev/sdb1       3.6T  3.3T  128G  ${sgr("1;31", "97%")} /mnt/backups`,
            "",
          ].join("\n"),
        );
        return 0;
      case "free":
        this.print(
          "               total        used        free      shared  buff/cache   available\nMem:            15Gi       5.2Gi       3.1Gi       412Mi       7.4Gi        10Gi\nSwap:          2.0Gi       128Mi       1.9Gi\n",
        );
        return 0;
      case "history":
        this.print(this.history.map((h, i) => `${String(i + 1).padStart(5)}  ${h}\n`).join(""));
        return 0;
      case "env":
      case "printenv":
        this.print(
          Object.entries(this.#env())
            .filter(([k]) => k !== "?")
            .map(([k, v]) => `${k}=${v}\n`)
            .join(""),
        );
        return 0;
      case "true":
        return 0;
      case "false":
        return 1;
      case "sudo":
        if (!args.length) {
          this.print("usage: sudo command\n");
          return 1;
        }
        return this.#run(args, job);
      case "systemctl":
      case "nginx":
      case "service":
        await sleep(400);
        if (job.cancelled) return 130;
        if (args.includes("-t")) this.print("nginx: the configuration file /etc/nginx/nginx.conf syntax is ok\nnginx: configuration file /etc/nginx/nginx.conf test is successful\n");
        else if (args[0] === "status") this.print(`${sgr("1;32", "●")} ${args[1] ?? "app.service"} - Application\n     Loaded: loaded (/etc/systemd/system/${args[1] ?? "app.service"}; enabled)\n     Active: ${sgr("1;32", "active (running)")} since Fri 2026-10-17 09:12:44 UTC; 5h ago\n`);
        return 0;
      case "colors":
        this.#colors();
        return 0;
      case "yes":
        return this.#stream(job, () => `${args.join(" ") || "y"}\n`, Infinity);
      case "flood": {
        const secs = Number(args[0] ?? 10);
        return this.#flood(job, Number.isFinite(secs) && secs > 0 ? secs : 10);
      }
      case "seq": {
        const n = Math.min(1_000_000, Math.max(0, Math.floor(Number(args[args.length - 1] ?? 10)) || 0));
        let i = 1;
        return this.#stream(
          job,
          () => {
            let s = "";
            for (let k = 0; k < 2000 && i <= n; k++) s += `${i++}\n`;
            return s;
          },
          Infinity,
          () => i > n,
        );
      }
      case "sleep":
      case "start-sleep": {
        const secs = Number(args[args.length - 1] ?? 1);
        if (!Number.isFinite(secs) || secs < 0) {
          this.print(`sleep: invalid time interval '${args[0] ?? ""}'\n`);
          return 1;
        }
        const end = Date.now() + secs * 1000;
        while (Date.now() < end && !job.cancelled && !this.closed) await sleep(Math.min(100, end - Date.now()));
        return job.cancelled ? 130 : 0;
      }
      case "title":
        this.title = args.join(" ") || null;
        return 0;
      case "bracketed-paste":
        if (args[0] === "on" || args[0] === "off") this.bracketed = args[0] === "on";
        this.print(`bracketed paste is ${this.bracketed ? "on" : "off"}\n`);
        return 0;
      case "drop-connection":
        if (!this.hostId) {
          this.print("drop-connection: only remote shells have a connection to drop\n");
          return 1;
        }
        this.print(dim("simulating a network failure…\n"));
        this.flush();
        await sleep(300);
        this.deps.dropConnection(this.hostId, "connection reset by peer");
        return -1;
      case "exit":
      case "logout": {
        const code = Number(args[0] ?? this.lastExit) || 0;
        this.print(this.remote ? "logout\n" : "");
        this.exit(code & 255);
        return -1;
      }
      case "vim":
      case "vi":
      case "nano":
      case "htop":
      case "top":
      case "less":
        this.print(`${cmd}: full-screen programs aren't simulated by the mock shell\n`);
        return 1;
      default:
        if (this.win) this.print(`${sgr("31", `${cmd} : The term '${cmd}' is not recognized as a name of a cmdlet, function, script file, or executable program.`)}\n`);
        else this.print(`${this.bsd ? "sh" : "bash"}: ${cmd}: command not found\n`);
        return 127;
    }
  }

  #ls(args: string[]): number {
    const flags = args.filter((a) => a.startsWith("-")).join("");
    const all = flags.includes("a");
    const long = flags.includes("l");
    const targets = args.filter((a) => !a.startsWith("-"));
    let code = 0;
    const list = targets.length ? targets : ["."];
    list.forEach((t, ti) => {
      const path = this.#abs(t);
      const r = tryResolve(this.m, path, true);
      if (!r) {
        this.print(`ls: cannot access '${t}': No such file or directory\n`);
        code = 2;
        return;
      }
      if (r.node.kind !== "dir") {
        this.print(`${t}\n`);
        return;
      }
      if (r.node.denied) {
        this.print(`ls: cannot open directory '${t}': Permission denied\n`);
        code = 2;
        return;
      }
      if (list.length > 1) this.print(`${ti ? "\n" : ""}${t}:\n`);
      const nodes = [...kids(r.node).values()]
        .filter((n) => all || !(n.name.startsWith(".") || n.hidden))
        .sort((a, b) => a.name.replace(/^\./, "").localeCompare(b.name.replace(/^\./, ""), undefined, { sensitivity: "base" }));
      const target = (n: Node) => (n.kind === "symlink" ? (tryResolve(this.m, `${r.real}/${n.name}`, true)?.node ?? null) : null);
      if (long) {
        const total = nodes.reduce((s, n) => s + Math.ceil((n.kind === "file" ? n.size : 4096) / 1024), 0);
        let out = `total ${total}\n`;
        const sizes = nodes.map((n) => (n.kind === "file" ? (flags.includes("h") ? humanSize(n.size) : String(Math.round(n.size))) : n.kind === "dir" ? "4096" : String(n.target.length)));
        const sw = Math.max(...sizes.map((s) => s.length), 1);
        const ow = Math.max(...nodes.map((n) => (n.owner ?? "-").length), 1);
        const gw = Math.max(...nodes.map((n) => (n.group ?? "-").length), 1);
        nodes.forEach((n, i) => {
          const color = this.#colorFor(n, target(n));
          const nm = color ? sgr(color, n.name) : n.name;
          const link = n.kind === "symlink" ? ` -> ${n.target}` : "";
          out += `${permString(n)} ${n.kind === "dir" ? 2 : 1} ${(n.owner ?? "-").padEnd(ow)} ${(n.group ?? "-").padEnd(gw)} ${sizes[i].padStart(sw)} ${lsDate(n.modified)} ${nm}${link}\n`;
        });
        this.print(out);
      } else {
        this.#columns(nodes.map((n) => ({ text: n.name, color: this.#colorFor(n, target(n)) })));
      }
    });
    return code;
  }

  #cd(arg: string | undefined): number {
    let target = arg ?? this.m.home;
    if (target === "-") {
      if (!this.oldPwd) {
        this.print("cd: OLDPWD not set\n");
        return 1;
      }
      target = this.oldPwd;
      this.print(`${this.display(target)}\n`);
    }
    const path = this.#abs(target);
    const r = tryResolve(this.m, path, true);
    if (!r) {
      this.print(`cd: ${arg}: No such file or directory\n`);
      return 1;
    }
    if (r.node.kind !== "dir") {
      this.print(`cd: ${arg}: Not a directory\n`);
      return 1;
    }
    if (r.node.denied) {
      this.print(`cd: ${arg}: Permission denied\n`);
      return 1;
    }
    this.oldPwd = this.cwd;
    this.cwd = path;
    return 0;
  }

  async #readText(arg: string, max = 64 * 1024): Promise<string | null> {
    const path = this.#abs(arg);
    const r = tryResolve(this.m, path, true);
    if (!r) {
      this.print(`${arg}: No such file or directory\n`);
      return null;
    }
    if (r.node.kind === "dir") {
      this.print(`${arg}: Is a directory\n`);
      return null;
    }
    if (r.node.kind !== "file") return null;
    const data = await readContent(r.node.content, r.node.name, r.node.size, max);
    if (data.subarray(0, 512).some((b) => b === 0)) {
      this.print(`${arg}: binary file (${humanSize(r.node.size)}B) -- not printed\n`);
      return null;
    }
    return dec.decode(data);
  }

  async #cat(args: string[]): Promise<number> {
    if (!args.length) {
      this.print("cat: reading stdin isn't supported by the mock shell\n");
      return 1;
    }
    let code = 0;
    for (const a of args) {
      const text = await this.#readText(a);
      if (text === null) {
        code = 1;
        continue;
      }
      this.print(text.endsWith("\n") || !text ? text : text + "\n");
    }
    return code;
  }

  async #headTail(cmd: "head" | "tail", args: string[]): Promise<number> {
    let n = 10;
    const files: string[] = [];
    for (let i = 0; i < args.length; i++) {
      if (args[i] === "-n") n = Number(args[++i]) || 10;
      else if (/^-\d+$/.test(args[i])) n = Number(args[i].slice(1));
      else if (args[i] !== "-f") files.push(args[i]);
    }
    if (!files.length) {
      this.print(`${cmd}: reading stdin isn't supported by the mock shell\n`);
      return 1;
    }
    for (const f of files) {
      const text = await this.#readText(f, 256 * 1024);
      if (text === null) return 1;
      const lines = text.split("\n");
      if (lines[lines.length - 1] === "") lines.pop();
      // Generated logs are cut at 256 KiB, so drop a possibly partial last line.
      if (cmd === "tail" && text.length >= 256 * 1024 - 1) lines.pop();
      const pick = cmd === "head" ? lines.slice(0, n) : lines.slice(-n);
      this.print(pick.map((l) => l + "\n").join(""));
    }
    return 0;
  }

  #colors() {
    const w = Math.max(16, this.cols);
    let out = `${bold("Standard and bright colors")}\n`;
    for (const base of [40, 100]) {
      for (let i = 0; i < 8; i++) {
        const n = base === 40 ? i : i + 8;
        out += `\x1b[${base + i}m\x1b[${n === 0 || n === 8 ? 97 : 30}m ${String(n).padStart(2)} \x1b[0m`;
      }
      out += "\n";
    }
    out += "\n";
    for (let i = 0; i < 8; i++) out += `\x1b[${30 + i}m${["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"][i].padEnd(8)}\x1b[0m `;
    out += "\n";
    for (let i = 0; i < 8; i++) out += `\x1b[${90 + i}m${("bright" + i).padEnd(8)}\x1b[0m `;
    out += `\n\n${bold("256-color cube")}\n`;
    for (let row = 0; row < 6; row++) {
      for (let g = 0; g < 6; g++) {
        for (let b = 0; b < 6; b++) {
          const n = 16 + row * 36 + g * 6 + b;
          out += `\x1b[48;5;${n}m  `;
        }
        out += "\x1b[0m ";
      }
      out += "\n";
    }
    for (let n = 232; n < 256; n++) out += `\x1b[48;5;${n}m  `;
    out += `\x1b[0m\n\n${bold("Truecolor")}\n`;
    const span = Math.min(w - 1, 120);
    const ramp = (f: (t: number) => [number, number, number]) => {
      let s = "";
      for (let x = 0; x < span; x++) {
        const t = x / (span - 1);
        const [r1, g1, b1] = f(t);
        const [r2, g2, b2] = f(Math.min(1, t + 0.5 / span));
        s += `\x1b[38;2;${r1};${g1};${b1}m\x1b[48;2;${r2};${g2};${b2}m▀`;
      }
      return s + "\x1b[0m\n";
    };
    const hsl = (h: number): [number, number, number] => {
      const f = (n: number) => {
        const k = (n + h * 12) % 12;
        return Math.round(255 * (0.5 - 0.5 * Math.max(-1, Math.min(k - 3, 9 - k, 1))));
      };
      return [f(0), f(8), f(4)];
    };
    out += ramp(hsl);
    out += ramp((t) => [Math.round(45 + t * 0), Math.round(212 - t * 113), Math.round(191 + t * 44)]);
    out += ramp((t) => [Math.round(255 * t), Math.round(255 * t), Math.round(255 * t)]);
    out += `\n${bold("Styles")}  ${bold("bold")}  ${dim("dim")}  \x1b[3mitalic\x1b[0m  \x1b[4munderline\x1b[0m  \x1b[4:3mcurly\x1b[0m  \x1b[9mstrike\x1b[0m  \x1b[7minverse\x1b[0m  \x1b[53moverline\x1b[0m\n`;
    out += `Unicode  ✓ ✗ → ⚡ 🚀 日本語 │ ├── └── ▁▂▃▄▅▆▇█\n`;
    this.print(out);
  }

  /** Writes `next()` chunks as fast as the UI plausibly drains them. */
  async #stream(job: { cancelled: boolean }, next: () => string, maxMs: number, done?: () => boolean): Promise<number> {
    const start = Date.now();
    while (!job.cancelled && !this.closed && Date.now() - start < maxMs && !done?.()) {
      let chunk = "";
      while (chunk.length < 24 * 1024 && !done?.()) chunk += next();
      this.print(chunk);
      this.flush();
      await new Promise<void>((r) => setTimeout(r, 0));
    }
    return job.cancelled ? 130 : 0;
  }

  async #flood(job: { cancelled: boolean }, secs: number): Promise<number> {
    const levels = [
      ["INFO ", "32"],
      ["DEBUG", "2"],
      ["WARN ", "33"],
      ["ERROR", "1;31"],
      ["TRACE", "36"],
    ] as const;
    const words = ["request", "handled", "cache", "miss", "upstream", "latency", "retry", "connection", "pool", "worker", "flushed", "queue", "shard", "commit", "snapshot", "gc", "tick"];
    let seq = 0;
    let bytes = 0;
    const start = Date.now();
    const code = await this.#stream(
      job,
      () => {
        seq++;
        const [lvl, color] = levels[seq % 7 === 0 ? 2 : seq % 31 === 0 ? 3 : seq % 3 === 0 ? 1 : seq % 11 === 0 ? 4 : 0];
        const msg = Array.from({ length: 4 + (seq % 7) }, (_, k) => words[(seq * 7 + k * 13) % words.length]).join(" ");
        const line = `\x1b[2m2026-10-17T14:${String(Math.floor(seq / 6000) % 60).padStart(2, "0")}:${String(Math.floor(seq / 100) % 60).padStart(2, "0")}.${String(seq % 1000).padStart(3, "0")}Z\x1b[0m \x1b[${color}m${lvl}\x1b[0m \x1b[35mworker-${seq % 8}\x1b[0m ${msg} \x1b[2mid=${(seq * 2654435761 >>> 0).toString(16)} took=${seq % 97}ms\x1b[0m\n`;
        bytes += line.length;
        return line;
      },
      secs * 1000,
    );
    const elapsed = (Date.now() - start) / 1000;
    if (code === 0) this.print(`${dim(`flood: ${seq.toLocaleString()} lines, ${(bytes / 1048576).toFixed(1)} MB in ${elapsed.toFixed(1)} s`)}\n`);
    return code;
  }

  // ------------------------------------------------------------ lifecycle

  exit(code: number | null, reason: string | null = null) {
    if (this.closed) return;
    this.flush();
    this.job?.cancel();
    this.job = null;
    this.closed = true;
    // terminalExit follows the last data chunk.
    setTimeout(() => this.deps.emit({ type: "terminalExit", terminalId: this.id, exitCode: code, reason }), 0);
  }

  kill() {
    this.job?.cancel();
    this.job = null;
    this.closed = true;
    if (this.#flushTimer) clearTimeout(this.#flushTimer);
  }
}

const COMMANDS = [
  "bracketed-paste", "cat", "cd", "clear", "colors", "date", "df", "drop-connection", "echo", "env", "exit", "flood",
  "free", "head", "help", "history", "hostname", "ls", "pwd", "seq", "sleep", "sudo", "systemctl", "tail", "title",
  "uname", "uptime", "whoami", "yes",
];

function motd(s: Shell, host: Host): string {
  if (s.win) {
    return "PowerShell 7.4.5\n\n";
  }
  const when = new Date(Date.now() - 3 * 3600 * 1000).toString().slice(0, 24);
  if (s.bsd) return `FreeBSD 14.1-RELEASE (GENERIC) #0\n\nWelcome to ${host.name}!\n\nLast login: ${when} from 192.168.1.20\n`;
  return `Linux ${s.hostname} 6.1.0-26-amd64 #1 SMP PREEMPT_DYNAMIC Debian 6.1.112-1 x86_64\n\nThe programs included with the Debian GNU/Linux system are free software.\n\nLast login: ${when} from 192.168.1.20\n`;
}

export function createTerminals(deps: ShellDeps): { methods: TerminalMethods; hostDisconnected(id: HostId, reason: string): void } {
  const shells = new Map<string, Shell>();
  let seq = 0;

  const methods: TerminalMethods = {
    async terminalOpen(spec, onData): Promise<TerminalInfo> {
      let os: RemoteCapabilities["os"] = "linux";
      let host: Host | undefined;
      if (spec.hostId !== null) {
        host = deps.host(spec.hostId);
        if (!host) throw err("unknownHost", `unknown host: ${spec.hostId}`);
        if (!deps.isConnected(spec.hostId)) throw err("notConnected", `not connected to ${host.name}`);
        const caps = deps.capabilities(spec.hostId);
        if (caps && !caps.exec) throw err("unsupported", `${host.name} only allows SFTP; shell sessions are disabled on this server`);
        os = caps?.os ?? "linux";
        await sleep(250 + Math.random() * 250);
        if (!deps.isConnected(spec.hostId)) throw err("notConnected", `not connected to ${host.name}`);
      } else {
        await sleep(40);
      }
      const id = `term-${Date.now().toString(36)}-${(seq++).toString(36)}`;
      const shell = new Shell(id, spec.hostId, deps, onData, { cols: spec.cols, rows: spec.rows, cwd: spec.cwd, os });
      shells.set(id, shell);
      setTimeout(() => {
        if (shell.closed) return;
        if (host) shell.print(motd(shell, host));
        shell.print(`${dim("Patchbay mock shell -- type help for commands")}\n`);
        shell.prompt();
      }, 30);
      const shellPath = shell.win ? "powershell.exe" : shell.bsd ? "/bin/sh" : os === "macOs" ? "/bin/zsh" : "/bin/bash";
      return { id, hostId: spec.hostId, shell: shellPath };
    },
    async terminalWrite(id, data) {
      const s = shells.get(id);
      if (!s || s.closed) throw err("notFound", `no such terminal: ${id}`);
      s.write(data);
    },
    async terminalResize(id, cols, rows) {
      const s = shells.get(id);
      if (!s) return;
      s.cols = Math.max(10, cols);
      s.rows = Math.max(4, rows);
    },
    async terminalAck() {},
    async terminalClose(id) {
      const s = shells.get(id);
      if (!s) return;
      s.kill();
      shells.delete(id);
    },
  };

  return {
    methods,
    hostDisconnected(id, reason) {
      for (const [tid, s] of shells) {
        if (s.hostId !== id) continue;
        s.exit(null, reason);
        shells.delete(tid);
      }
    },
  };
}
