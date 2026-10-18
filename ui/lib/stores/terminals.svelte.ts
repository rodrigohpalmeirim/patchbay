// Live terminal sessions, one per pane (keyed by pane id).
//
// A session owns its xterm instance and the element xterm renders into, so
// the view components can be re-arranged freely: panes attach the element
// on mount and nothing is torn down until the pane or tab is closed.
//
// Output path: backend `onData` chunks go straight into `term.write` (xterm
// buffers and parses in time slices); nothing per-chunk touches Svelte state.
// Input path: keystrokes are coalesced and sent one write at a time so they
// arrive in order even though IPC calls may complete out of order.

import type { FitAddon } from "@xterm/addon-fit";
import type { ISearchOptions, SearchAddon } from "@xterm/addon-search";
import type { IDisposable, Terminal } from "@xterm/xterm";
import { SvelteMap } from "svelte/reactivity";
import { isTauri } from "../api";
import type { TermAction } from "../terminal/keys";
import { passesToApp, terminalAction } from "../terminal/keys";
import { disableWebgl, loadWebgl, loadXterm, searchDecorations, terminalTheme } from "../terminal/xterm";
import type { CommandError, CoreEvent, HostId } from "../types";
import { toCommandError } from "../util/errors";
import { isMac } from "../util/platform";
import { attempt, call } from "./backend";
import { connections } from "./connections.svelte";
import { DEFAULT_TERMINAL_FONT, settings } from "./settings.svelte";
import { toasts } from "./toasts.svelte";
import { ui } from "./ui.svelte";
import { readClipboard, writeClipboard } from "../util/clipboard";

export type SessionStatus =
  /** xterm is loading. */
  | "loading"
  /** Remote host isn't connected yet. */
  | "waiting"
  | "opening"
  | "running"
  | "exited"
  | "failed";

export interface ExitInfo {
  code: number | null;
  reason: string | null;
}

export interface SessionHooks {
  /** A terminal shortcut was pressed inside this pane. */
  onAction(action: TermAction): void;
  onFocus(): void;
  /** OSC 7 reported a new working directory. */
  onCwd(cwd: string): void;
}

export interface LinkHover {
  x: number;
  y: number;
  uri: string;
}

const RESIZE_DEBOUNCE = 80;
const PASTE_CONFIRM_PREVIEW = 40;

function parseOsc7(data: string): string | null {
  try {
    const url = new URL(data);
    if (url.protocol !== "file:") return null;
    return decodeURIComponent(url.pathname) || "/";
  } catch {
    return null;
  }
}

async function openExternal(uri: string) {
  if (!/^(https?|mailto):/i.test(uri)) return;
  if (isTauri) {
    try {
      const { openUrl } = await import("@tauri-apps/plugin-opener");
      await openUrl(uri);
    } catch (e) {
      toasts.error(toCommandError(e), "Couldn't open link");
    }
  } else {
    window.open(uri, "_blank", "noopener,noreferrer");
  }
}

export class TermSession {
  status = $state<SessionStatus>("loading");
  error = $state<CommandError | null>(null);
  exit = $state<ExitInfo | null>(null);
  /** Window title set by the shell (OSC 0/2). */
  title = $state<string | null>(null);
  /** Working directory reported by the shell (OSC 7), else the start directory. */
  cwd = $state<string | null>(null);
  shell = $state<string | null>(null);
  /** A shell has run in this pane before (restarts keep the old output visible). */
  started = $state(false);
  hasSelection = $state(false);
  findOpen = $state(false);
  /** Bumped to (re)focus the find input. */
  findTick = $state(0);
  searchResult = $state<{ index: number; count: number } | null>(null);
  /** Bumped on a visual bell. */
  bellTick = $state(0);
  linkHover = $state<LinkHover | null>(null);

  /** Element xterm renders into; panes re-parent it. */
  readonly el: HTMLDivElement;
  term: Terminal | null = null;
  terminalId: string | null = null;

  #fit: FitAddon | null = null;
  #search: SearchAddon | null = null;
  #disposables: IDisposable[] = [];
  #ro: ResizeObserver | null = null;
  #gen = 0;
  #disposed = false;
  #initStarted = false;
  #fitFrame = 0;
  #resizeTimer: ReturnType<typeof setTimeout> | undefined;
  #sentSize = { cols: 0, rows: 0 };
  #writeBuf = "";
  #writing = false;
  /** Rendered output bytes not yet acknowledged to the backend. */
  #unacked = 0;
  #ackTimer: ReturnType<typeof setTimeout> | undefined;
  #copyTimer: ReturnType<typeof setTimeout> | undefined;
  #disconnectTimer: ReturnType<typeof setTimeout> | undefined;

  constructor(
    readonly paneId: string,
    readonly tabId: string,
    readonly hostId: HostId | null,
    startCwd: string | null,
    private hooks: SessionHooks,
  ) {
    this.cwd = startCwd;
    this.el = document.createElement("div");
    this.el.className = "term-host";
  }

  get disposed(): boolean {
    return this.#disposed;
  }

  // -------------------------------------------------------------- setup

  /** Puts the terminal element into `container`, creating xterm on first use. */
  attach(container: HTMLElement) {
    if (this.#disposed) return;
    if (this.el.parentElement !== container) container.appendChild(this.el);
    if (!this.#initStarted) {
      this.#initStarted = true;
      void this.#init();
    } else {
      this.scheduleFit();
    }
  }

  async #init() {
    let x: Awaited<ReturnType<typeof loadXterm>>;
    try {
      x = await loadXterm();
    } catch (e) {
      this.status = "failed";
      this.error = { kind: "other", message: `the terminal component failed to load (${toCommandError(e).message})` };
      return;
    }
    if (this.#disposed) return;
    const prefs = settings.terminal;
    const dark = settings.resolvedTheme === "dark";
    const os = this.hostId ? connections.get(this.hostId).capabilities?.os : null;
    const windowsPty = os === "windows" || (!this.hostId && /Win/.test(navigator.platform));
    const term = new x.Terminal({
      allowProposedApi: true,
      fontFamily: prefs.fontFamily.trim() || DEFAULT_TERMINAL_FONT,
      fontSize: prefs.fontSize,
      lineHeight: prefs.lineHeight,
      cursorStyle: prefs.cursorStyle,
      cursorBlink: prefs.cursorBlink,
      cursorInactiveStyle: "outline",
      scrollback: prefs.scrollback,
      theme: terminalTheme(dark),
      rightClickSelectsWord: isMac,
      macOptionClickForcesSelection: true,
      ...(windowsPty ? { windowsPty: { backend: "conpty" as const } } : {}),
      linkHandler: {
        activate: (e, uri) => this.#activateLink(e, uri),
        hover: (e, uri) => this.#hoverLink(e, uri),
        leave: () => (this.linkHover = null),
      },
    });
    this.term = term;

    this.#fit = new x.FitAddon();
    term.loadAddon(this.#fit);
    this.#search = new x.SearchAddon({ highlightLimit: 2000 });
    term.loadAddon(this.#search);
    term.loadAddon(
      new x.WebLinksAddon((e, uri) => this.#activateLink(e, uri), {
        hover: (e, uri) => this.#hoverLink(e, uri),
        leave: () => (this.linkHover = null),
      }),
    );
    term.loadAddon(new x.Unicode11Addon());
    term.unicode.activeVersion = "11";

    term.open(this.el);
    await this.#loadRenderer();
    if (this.#disposed) return;

    term.attachCustomKeyEventHandler((e) => this.#onKey(e));
    const d = this.#disposables;
    d.push(term.onData((data) => this.write(data)));
    d.push(term.onBinary((data) => this.write(data)));
    d.push(term.onTitleChange((t) => (this.title = cleanTitle(t))));
    d.push(term.onResize(() => this.#queueResize()));
    d.push(term.onBell(() => {
      if (settings.terminal.bell === "visual") this.bellTick++;
    }));
    d.push(term.onSelectionChange(() => this.#onSelection()));
    d.push(
      term.parser.registerOscHandler(7, (data) => {
        const cwd = parseOsc7(data);
        if (cwd && cwd !== this.cwd) {
          this.cwd = cwd;
          this.hooks.onCwd(cwd);
        }
        return true;
      }),
    );
    d.push(this.#search.onDidChangeResults((r) => (this.searchResult = { index: r.resultIndex, count: r.resultCount })));
    term.textarea?.addEventListener("focus", () => this.hooks.onFocus());
    // Paste events (menu, middle click, Shift+Insert, ⌘V) go through our
    // multi-line check instead of xterm's own handler.
    this.el.addEventListener(
      "paste",
      (e) => {
        e.preventDefault();
        e.stopPropagation();
        void this.paste(e.clipboardData?.getData("text/plain") ?? "");
      },
      true,
    );

    this.#ro = new ResizeObserver(() => this.scheduleFit());
    this.#ro.observe(this.el);
    this.fitNow();
    void this.open();
  }

  async #loadRenderer() {
    const WebglAddon = await loadWebgl();
    if (!WebglAddon || !this.term || this.#disposed) return;
    try {
      const addon = new WebglAddon();
      addon.onContextLoss(() => {
        // GPU reset etc.: fall back to the DOM renderer.
        addon.dispose();
        disableWebgl();
      });
      this.term.loadAddon(addon);
    } catch {
      disableWebgl();
    }
  }

  /** Applies settings + theme changes to a live terminal. */
  applyPrefs() {
    const term = this.term;
    if (!term) return;
    const p = settings.terminal;
    const dark = settings.resolvedTheme === "dark";
    const family = p.fontFamily.trim() || DEFAULT_TERMINAL_FONT;
    const o = term.options;
    if (o.fontFamily !== family) o.fontFamily = family;
    if (o.fontSize !== p.fontSize) o.fontSize = p.fontSize;
    if (o.lineHeight !== p.lineHeight) o.lineHeight = p.lineHeight;
    if (o.cursorStyle !== p.cursorStyle) o.cursorStyle = p.cursorStyle;
    if (o.cursorBlink !== p.cursorBlink) o.cursorBlink = p.cursorBlink;
    if (o.scrollback !== p.scrollback) o.scrollback = p.scrollback;
    o.theme = terminalTheme(dark);
    this.scheduleFit();
  }

  // ------------------------------------------------------------- sizing

  /** Fits xterm to its element now. False when hidden / not ready. */
  fitNow(): boolean {
    const term = this.term;
    if (!term || !this.#fit || this.#disposed) return false;
    if (this.el.clientWidth < 20 || this.el.clientHeight < 10) return false;
    const dims = this.#fit.proposeDimensions();
    if (!dims || !Number.isFinite(dims.cols) || !Number.isFinite(dims.rows) || dims.cols < 2 || dims.rows < 1) return false;
    if (dims.cols !== term.cols || dims.rows !== term.rows) term.resize(dims.cols, dims.rows);
    return true;
  }

  /** Coalesces fits to one per frame (live while dragging dividers). */
  scheduleFit() {
    if (this.#fitFrame) return;
    this.#fitFrame = requestAnimationFrame(() => {
      this.#fitFrame = 0;
      this.fitNow();
    });
  }

  #queueResize() {
    clearTimeout(this.#resizeTimer);
    this.#resizeTimer = setTimeout(() => this.#sendSize(), RESIZE_DEBOUNCE);
  }

  #sendSize() {
    const term = this.term;
    const id = this.terminalId;
    if (!term || !id || this.status !== "running") return;
    const { cols, rows } = term;
    if (cols === this.#sentSize.cols && rows === this.#sentSize.rows) return;
    this.#sentSize = { cols, rows };
    void attempt((b) => b.terminalResize(id, cols, rows), { toast: false });
  }

  // ------------------------------------------------------------ process

  /** Opens the shell (no-op unless idle/waiting/exited/failed). */
  async open() {
    const term = this.term;
    if (!term || this.#disposed || this.status === "opening" || this.status === "running") return;
    const hostId = this.hostId;
    if (hostId) {
      const conn = connections.get(hostId);
      if (conn.phase !== "connected") {
        this.status = "waiting";
        return;
      }
      if (conn.capabilities && !conn.capabilities.exec) {
        this.status = "failed";
        this.error = { kind: "unsupported", message: "this server only allows SFTP, so it can't run a shell" };
        return;
      }
    }
    const gen = ++this.#gen;
    this.status = "opening";
    this.error = null;
    this.exit = null;
    this.fitNow();
    try {
      const info = await call(
        (b) =>
          b.terminalOpen({ hostId, cwd: this.cwd, cols: term.cols, rows: term.rows }, (data) => {
            if (gen !== this.#gen || !data.length) return;
            term.write(data, () => {
              if (gen === this.#gen) this.#ack(data.length);
            });
          }),
        { toast: false },
      );
      if (gen !== this.#gen || this.#disposed) {
        void attempt((b) => b.terminalClose(info.id), { toast: false });
        return;
      }
      this.terminalId = info.id;
      this.shell = info.shell;
      this.status = "running";
      // Input typed (or terminal replies such as cursor-position reports
      // that ConPTY waits for) while the shell was starting.
      if (this.#writeBuf && !this.#writing) void this.#flushWrites();
      this.#ack(0);
      this.started = true;
      this.#sentSize = { cols: term.cols, rows: term.rows };
      terminals.register(info.id, this);
      // The pane may have been resized while opening.
      if (this.fitNow()) this.#sendSize();
    } catch (e) {
      if (gen !== this.#gen || this.#disposed) return;
      const error = toCommandError(e);
      if (error.kind === "notConnected") {
        this.status = "waiting";
        return;
      }
      this.status = "failed";
      this.error = error;
    }
  }

  /** The shell ended (backend `terminalExit`). */
  handleExit(code: number | null, reason: string | null) {
    if (this.status !== "running" && this.status !== "opening") return;
    clearTimeout(this.#disconnectTimer);
    if (this.terminalId) terminals.unregister(this.terminalId);
    this.terminalId = null;
    this.#gen++;
    this.#writeBuf = "";
    this.#unacked = 0;
    clearTimeout(this.#ackTimer);
    this.status = "exited";
    this.exit = { code, reason };
    this.linkHover = null;
  }

  /** Fallback in case the backend doesn't report the end of a shell on disconnect. */
  hostDisconnected(reason: string | null) {
    if (this.status !== "running") return;
    clearTimeout(this.#disconnectTimer);
    this.#disconnectTimer = setTimeout(() => {
      if (this.status === "running") this.handleExit(null, reason ?? "connection closed");
    }, 1500);
  }

  /** Starts a new shell in the same pane (same directory), connecting first if needed. */
  restart() {
    const term = this.term;
    if (!term || this.#disposed || this.status === "running" || this.status === "opening") return;
    // Leave whatever full-screen state the old program left behind.
    const alt = term.buffer.active.type === "alternate" ? "\x1b[?1049l" : "";
    if (this.started) term.write(`${alt}\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?25h\x1b[0m\r\n`);
    this.exit = null;
    this.error = null;
    if (this.hostId) {
      const phase = connections.get(this.hostId).phase;
      if (phase !== "connected") {
        this.status = "waiting";
        if (phase === "idle" || phase === "error") void connections.connect(this.hostId);
        return;
      }
    }
    void this.open();
    term.focus();
  }

  // -------------------------------------------------------------- input

  /** Sends input to the shell, preserving order. Input while the shell is
   * starting is buffered and sent once it is up. */
  write(data: string) {
    if (this.status === "opening") {
      this.#writeBuf += data;
      return;
    }
    if (this.status !== "running" || !this.terminalId) return;
    this.#writeBuf += data;
    if (!this.#writing) void this.#flushWrites();
  }

  /** Batches output acknowledgements (backend flow control). */
  #ack(bytes: number) {
    this.#unacked += bytes;
    if (!this.terminalId || !this.#unacked) return;
    const flush = () => {
      clearTimeout(this.#ackTimer);
      this.#ackTimer = undefined;
      const id = this.terminalId;
      const n = this.#unacked;
      this.#unacked = 0;
      if (id && n) void attempt((b) => b.terminalAck(id, n), { toast: false });
    };
    if (this.#unacked >= 64 * 1024) flush();
    else this.#ackTimer ??= setTimeout(flush, 50);
  }

  async #flushWrites() {
    this.#writing = true;
    while (this.#writeBuf && this.terminalId && !this.#disposed) {
      const id = this.terminalId;
      const chunk = this.#writeBuf;
      this.#writeBuf = "";
      try {
        await call((b) => b.terminalWrite(id, chunk), { toast: false });
      } catch (e) {
        const error = toCommandError(e);
        if (id === this.terminalId && (error.kind === "notFound" || error.kind === "notConnected")) {
          this.handleExit(null, error.message);
        }
        break;
      }
    }
    this.#writing = false;
  }

  #onKey(e: KeyboardEvent): boolean {
    const action = terminalAction(e);
    if (action) {
      if (e.type === "keydown") this.#runAction(e, action);
      return false;
    }
    if (this.status !== "running") {
      // A dead pane: Enter restarts. Other keys still scroll etc.; input is dropped.
      const plainEnter = e.key === "Enter" && !e.ctrlKey && !e.altKey && !e.metaKey && !e.shiftKey;
      if (plainEnter && (this.status === "exited" || this.status === "failed" || this.status === "waiting")) {
        if (e.type === "keydown") {
          e.preventDefault();
          this.restart();
        }
        return false;
      }
    }
    // Let app shortcuts (tab switching, settings…) bubble to the window.
    return !passesToApp(e);
  }

  #runAction(e: KeyboardEvent, action: TermAction) {
    // Native clipboard paths: Shift+Insert / ⌘V produce a real paste event.
    if (action === "paste" && (e.key === "Insert" || isMac)) return;
    e.preventDefault();
    e.stopPropagation();
    this.hooks.onAction(action);
  }

  // ----------------------------------------------------------- clipboard

  #onSelection() {
    const has = this.term?.hasSelection() ?? false;
    if (has !== this.hasSelection) this.hasSelection = has;
    clearTimeout(this.#copyTimer);
    if (has && settings.terminal.copyOnSelect) {
      this.#copyTimer = setTimeout(() => void this.copy(false), 150);
    }
  }

  async copy(notify = false): Promise<boolean> {
    const text = this.term?.getSelection() ?? "";
    if (!text) return false;
    try {
      await writeClipboard(text);
      if (notify) toasts.success("Copied to clipboard");
      return true;
    } catch {
      toasts.show("error", "Couldn't access the clipboard");
      return false;
    }
  }

  async pasteFromClipboard() {
    let text: string;
    try {
      text = await readClipboard();
    } catch {
      toasts.show("error", "Couldn't read the clipboard", `Use ${isMac ? "⌘V" : "Shift+Insert"} to paste instead.`);
      return;
    }
    await this.paste(text);
  }

  /** Pastes text, asking first when several lines would run right away. */
  async paste(text: string) {
    const term = this.term;
    if (!text || !term || this.status !== "running") return;
    const lines = text.replace(/\r\n?/g, "\n").replace(/\n$/, "").split("\n");
    const runsImmediately = /[\r\n]/.test(text) && !term.modes.bracketedPasteMode;
    if (runsImmediately && settings.terminal.confirmMultilinePaste) {
      const n = Math.max(lines.length, 1);
      const { ok, checked } = await ui.confirmWithOption({
        title: n === 1 ? "Paste and run this line?" : `Paste ${n} lines?`,
        message:
          n === 1
            ? "The text ends with a line break, so the shell will run it as soon as it's pasted."
            : "This shell doesn't support bracketed paste, so each line runs as a separate command as soon as it's pasted.",
        confirmLabel: "Paste",
        details: lines.slice(0, PASTE_CONFIRM_PREVIEW).map((l) => l || " "),
        option: "Don't ask again",
      });
      if (!ok) {
        term.focus();
        return;
      }
      if (checked) settings.setTerminal({ confirmMultilinePaste: false });
      if (this.status !== "running") return;
    }
    term.paste(text);
    term.focus();
  }

  // ---------------------------------------------------------------- misc

  focus() {
    this.term?.focus();
  }

  selectAll() {
    this.term?.selectAll();
  }

  clear() {
    this.term?.clear();
  }

  /** Types `text` at the prompt (bracketed if the shell wants it), optionally pressing Enter. */
  insert(text: string, run: boolean) {
    const term = this.term;
    if (!term || this.status !== "running") return;
    const body = run ? text.replace(/[\r\n]+$/, "") : text;
    term.paste(body);
    if (run) this.write("\r");
    term.focus();
  }

  find(query: string, dir: "next" | "prev", opts: { caseSensitive: boolean; regex: boolean; incremental?: boolean }) {
    const s = this.#search;
    if (!s) return;
    if (!query) {
      s.clearDecorations();
      this.searchResult = null;
      return;
    }
    const dark = settings.resolvedTheme === "dark";
    const options: ISearchOptions = {
      caseSensitive: opts.caseSensitive,
      regex: opts.regex,
      incremental: opts.incremental,
      decorations: searchDecorations(dark),
    };
    let found = false;
    try {
      found = dir === "next" ? s.findNext(query, options) : s.findPrevious(query, options);
    } catch {
      found = false; // invalid regex
    }
    if (!found) this.searchResult = { index: -1, count: 0 };
  }

  closeFind() {
    this.#search?.clearDecorations();
    this.term?.clearSelection();
    this.searchResult = null;
    this.findOpen = false;
    this.focus();
  }

  #activateLink(e: MouseEvent, uri: string) {
    if (!(isMac ? e.metaKey : e.ctrlKey)) return;
    e.preventDefault();
    void openExternal(uri);
  }

  #hoverLink(e: MouseEvent, uri: string) {
    const r = this.el.getBoundingClientRect();
    this.linkHover = { x: e.clientX - r.left, y: e.clientY - r.top, uri };
  }

  dispose() {
    if (this.#disposed) return;
    this.#disposed = true;
    this.#gen++;
    clearTimeout(this.#resizeTimer);
    clearTimeout(this.#copyTimer);
    clearTimeout(this.#disconnectTimer);
    if (this.#fitFrame) cancelAnimationFrame(this.#fitFrame);
    const id = this.terminalId;
    if (id) {
      terminals.unregister(id);
      void attempt((b) => b.terminalClose(id), { toast: false });
    }
    this.terminalId = null;
    this.#ro?.disconnect();
    this.#disposables.forEach((d) => d.dispose());
    this.term?.dispose();
    this.term = null;
    this.el.remove();
  }
}

class Terminals {
  sessions = new SvelteMap<string, TermSession>();
  /** Tab whose focused pane should take keyboard focus once it's ready. */
  focusRequest = $state<string | null>(null);
  /** Terminal tab showing the snippet palette. */
  paletteFor = $state<string | null>(null);
  #byTerminalId = new Map<string, TermSession>();
  /** Exits that arrived before terminalOpen resolved. */
  #early = new Map<string, ExitInfo>();

  requestFocus(tabId: string) {
    this.focusRequest = tabId;
  }

  get(paneId: string | null | undefined): TermSession | undefined {
    return paneId ? this.sessions.get(paneId) : undefined;
  }

  ensure(paneId: string, tabId: string, hostId: HostId | null, cwd: string | null, hooks: SessionHooks): TermSession {
    let s = this.sessions.get(paneId);
    if (!s || s.disposed) {
      s = new TermSession(paneId, tabId, hostId, cwd, hooks);
      this.sessions.set(paneId, s);
    }
    return s;
  }

  register(terminalId: string, s: TermSession) {
    this.#byTerminalId.set(terminalId, s);
    const early = this.#early.get(terminalId);
    if (early) {
      this.#early.delete(terminalId);
      s.handleExit(early.code, early.reason);
    }
  }

  unregister(terminalId: string) {
    this.#byTerminalId.delete(terminalId);
  }

  handleExit(ev: Extract<CoreEvent, { type: "terminalExit" }>) {
    const s = this.#byTerminalId.get(ev.terminalId);
    if (s) {
      s.handleExit(ev.exitCode, ev.reason);
      return;
    }
    this.#early.set(ev.terminalId, { code: ev.exitCode, reason: ev.reason });
    setTimeout(() => this.#early.delete(ev.terminalId), 10_000);
  }

  /** A host's connection went away: its shells are gone. */
  hostDisconnected(hostId: HostId, reason: string | null) {
    for (const s of this.sessions.values()) if (s.hostId === hostId) s.hostDisconnected(reason);
  }

  dispose(paneId: string) {
    this.sessions.get(paneId)?.dispose();
    this.sessions.delete(paneId);
  }

  disposeTab(tabId: string) {
    for (const [id, s] of this.sessions) if (s.tabId === tabId) this.dispose(id);
  }
}

export const terminals = new Terminals();

/**
 * Tidies window titles set by the shell. Windows consoles report e.g.
 * `Administrator: C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe`;
 * show just `powershell` (and keep the elevation hint).
 */
function cleanTitle(raw: string): string | null {
  let title = raw.trim();
  const admin = /^Administrator:\s*/i.exec(title);
  if (admin) title = title.slice(admin[0].length);
  const exe = /^(?:[A-Za-z]:)?[\\/](?:[^\\/]+[\\/])*([^\\/]+?)(?:\.exe)?$/i.exec(title);
  if (exe && /^[A-Za-z]:|\\/.test(title)) title = exe[1];
  if (!title) return null;
  return admin ? `${title} (admin)` : title;
}
