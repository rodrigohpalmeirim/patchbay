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
//
// Moving a pane to another window keeps its shell: the source pauses the
// backend stream (`terminalDetach`, which says how many bytes were sent
// here), waits until they have arrived and been parsed, and serializes the
// screen and scrollback (`handOver`). The target window builds the pane
// from that snapshot, then attaches a new output channel
// (`terminalAttach`); output produced meanwhile waits in the backend. The
// source then `release`s its session without closing the shell, or, if the
// move failed, `resume`s it. See `windows.moveTab` and src-tauri/src/streams.rs.

import type { FitAddon } from "@xterm/addon-fit";
import type { ISearchOptions, SearchAddon } from "@xterm/addon-search";
import type { IDisposable, Terminal } from "@xterm/xterm";
import { SvelteMap } from "svelte/reactivity";
import { isTauri } from "../api";
import type { TermAction } from "../terminal/keys";
import { passesToApp, terminalAction } from "../terminal/keys";
import { disableWebgl, loadWebgl, loadXterm, searchDecorations, serializeTerminal, terminalTheme } from "../terminal/xterm";
import type { CommandError, CoreEvent, HostId } from "../types";
import { toCommandError } from "../util/errors";
import { isMac, isWindows } from "../util/platform";
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

/** A pane as handed to another window (see the header). */
export interface PaneHandover {
  /** Its shell, paused until the target attaches; null if none is running. */
  terminalId: string | null;
  /** Screen and scrollback as escape sequences (null: xterm never started). */
  screen: string | null;
  /** Size the screen was laid out in. */
  cols: number;
  rows: number;
  title: string | null;
  cwd: string | null;
  shell: string | null;
  started: boolean;
  /** How the shell ended, if it did (null with no terminal: start a new one). */
  exit: ExitInfo | null;
}

export interface LinkHover {
  x: number;
  y: number;
  uri: string;
}

const RESIZE_DEBOUNCE = 80;
const PASTE_CONFIRM_PREVIEW = 40;
/** How long a pane being moved waits for output already sent to it. */
const HANDOVER_DRAIN_TIMEOUT = 5000;

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
  /** Output bytes received on the current channel, and a waiter for more (`handOver`). */
  #bytesIn = 0;
  #onBytes: (() => void) | null = null;
  /** Output paused for a move to another window. */
  #detached = false;
  /** Moved here from another window: built from this once xterm is up. */
  #incoming: PaneHandover | null;

  constructor(
    readonly paneId: string,
    /** Terminal tab showing the pane; changes when the pane is moved to another tab. */
    public tabId: string,
    readonly hostId: HostId | null,
    startCwd: string | null,
    private hooks: SessionHooks,
    incoming: PaneHandover | null = null,
  ) {
    this.cwd = incoming?.cwd ?? startCwd;
    this.el = document.createElement("div");
    this.el.className = "term-host";
    this.#incoming = incoming;
    if (incoming) {
      this.title = incoming.title;
      this.shell = incoming.shell;
      this.started = incoming.started;
      // Ours from now on: closed if this pane goes before taking it over.
      if (incoming.terminalId) {
        this.terminalId = incoming.terminalId;
        terminals.register(incoming.terminalId, this);
      }
    }
  }

  get disposed(): boolean {
    return this.#disposed;
  }

  /** Moved to another tab (see `Terminals.adopt`): its view's hooks apply from now on. */
  rebind(tabId: string, hooks: SessionHooks) {
    this.tabId = tabId;
    this.hooks = hooks;
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
    const incoming = this.#incoming;
    const term = new x.Terminal({
      allowProposedApi: true,
      // A moved screen is restored at the size it was laid out in, then fitted.
      ...(incoming?.screen ? { cols: incoming.cols, rows: incoming.rows } : {}),
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
    if (incoming) {
      this.#incoming = null;
      void this.#takeOver(term, incoming);
      return;
    }
    this.fitNow();
    void this.open();
  }

  async #loadRenderer() {
    const WebglAddon = await loadWebgl();
    if (!WebglAddon || !this.term || this.#disposed) return;
    try {
      // preserveDrawingBuffer, on the WebKit webviews (Linux, macOS): there a
      // frame drawn without any other change on the page is not shown until
      // something else repaints (the canvas keeps showing the previous
      // frame). With a blinking cursor the next blink hides that, but once
      // the cursor stops blinking (a program sends DECRST 12, as `top` does
      // on exit; the setting is off; the pane is unfocused) the last output
      // stays invisible until the next key. WebView2 (Windows) presents every
      // frame, so it is spared the extra buffer copy per frame.
      const addon = new WebglAddon(!isWindows);
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
        (b) => b.terminalOpen({ hostId, cwd: this.cwd, cols: term.cols, rows: term.rows }, this.#sink(gen, term)),
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

  /** Handles the output channel of shell `gen`. */
  #sink(gen: number, term: Terminal): (data: Uint8Array) => void {
    this.#bytesIn = 0;
    return (data) => {
      if (gen !== this.#gen || !data.length) return;
      this.#bytesIn += data.length;
      this.#onBytes?.();
      term.write(data, () => {
        if (gen === this.#gen) this.#ack(data.length);
      });
    };
  }

  // ------------------------------------------------- moving to a window

  /**
   * Prepares moving this pane to another window (see the header): pauses
   * the shell's output here and snapshots the screen once everything sent
   * here is in it. Follow up with `release` (moved) or `resume` (not moved).
   */
  async handOver(): Promise<PaneHandover> {
    const term = this.term;
    const id = this.status === "running" ? this.terminalId : null;
    let terminalId: string | null = null;
    if (id) {
      const sent = await attempt((b) => b.terminalDetach(id), { toast: false });
      // Not available: the shell has just ended.
      if (sent !== undefined) {
        terminalId = id;
        this.#detached = true;
      }
      if (sent !== undefined && !(await this.#received(sent))) {
        this.resume();
        throw new Error("the terminal's output didn't arrive");
      }
    }
    let screen: string | null = null;
    try {
      if (term) {
        await new Promise<void>((resolve) => term.write("", resolve));
        screen = await serializeTerminal(term);
      }
    } catch (e) {
      this.resume();
      throw e;
    }
    const ended = !terminalId && (this.status === "running" || this.status === "exited");
    return {
      terminalId,
      screen,
      cols: term?.cols ?? 80,
      rows: term?.rows ?? 24,
      title: this.title,
      cwd: this.cwd,
      shell: this.shell,
      started: this.started,
      exit: ended ? (this.exit ?? { code: null, reason: null }) : null,
    };
  }

  /** Waits until `bytes` bytes of output have arrived here. False on timeout. */
  #received(bytes: number): Promise<boolean> {
    if (this.#bytesIn >= bytes) return Promise.resolve(true);
    return new Promise((resolve) => {
      const done = (ok: boolean) => {
        clearTimeout(timer);
        this.#onBytes = null;
        resolve(ok);
      };
      const timer = setTimeout(() => done(false), HANDOVER_DRAIN_TIMEOUT);
      this.#onBytes = () => {
        if (this.#bytesIn >= bytes) done(true);
      };
    });
  }

  /** The move didn't happen: the shell's output comes here again. */
  resume() {
    const id = this.terminalId;
    if (!this.#detached || !id) return;
    this.#detached = false;
    void attempt((b) => b.terminalResume(id), { toast: false });
  }

  /** Moved to another window: lets go of the shell (without closing it) and disposes the rest. */
  release() {
    if (this.terminalId) terminals.unregister(this.terminalId);
    this.terminalId = null;
    clearTimeout(this.#ackTimer);
    this.#unacked = 0;
    this.dispose();
  }

  /** Builds a pane moved here from another window: its screen, then its shell. */
  async #takeOver(term: Terminal, h: PaneHandover) {
    if (h.screen) {
      const screen = h.screen;
      await new Promise<void>((resolve) => term.write(screen, resolve));
    }
    if (this.#disposed) return;
    this.fitNow();
    const id = h.terminalId;
    if (!id) {
      // No shell came along: show how it ended, or start one.
      if (h.exit) {
        this.status = "exited";
        this.exit = h.exit;
      } else {
        void this.open();
      }
      return;
    }
    const gen = ++this.#gen;
    this.status = "opening";
    try {
      await call((b) => b.terminalAttach(id, this.#sink(gen, term)), { toast: false });
    } catch {
      if (gen !== this.#gen || this.#disposed) return;
      terminals.unregister(id);
      this.terminalId = null;
      this.status = "exited";
      this.exit = { code: null, reason: "the shell was lost while moving it to this window" };
      return;
    }
    if (gen !== this.#gen || this.#disposed) return;
    this.status = "running";
    if (this.#writeBuf && !this.#writing) void this.#flushWrites();
    // The shell still has the old window's size.
    this.#sentSize = { cols: 0, rows: 0 };
    this.fitNow();
    this.#sendSize();
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
  /** Panes moved here from another window, until their sessions are created. */
  #incoming = new Map<string, PaneHandover>();

  requestFocus(tabId: string) {
    this.focusRequest = tabId;
  }

  get(paneId: string | null | undefined): TermSession | undefined {
    return paneId ? this.sessions.get(paneId) : undefined;
  }

  /** The pane's session, created if needed; an existing one takes the (possibly new) tab's hooks. */
  ensure(paneId: string, tabId: string, hostId: HostId | null, cwd: string | null, hooks: SessionHooks): TermSession {
    let s = this.sessions.get(paneId);
    if (!s || s.disposed) {
      const incoming = this.#incoming.get(paneId) ?? null;
      this.#incoming.delete(paneId);
      s = new TermSession(paneId, tabId, hostId, cwd, hooks, incoming);
      this.sessions.set(paneId, s);
    } else {
      s.rebind(tabId, hooks);
    }
    return s;
  }

  /**
   * Hands panes over to another tab, keeping their shells. Must happen before
   * the panes leave the old tab's layout, which disposes sessions it no longer shows.
   */
  adopt(paneIds: string[], tabId: string) {
    for (const id of paneIds) {
      const s = this.sessions.get(id);
      if (s) s.tabId = tabId;
    }
  }

  /**
   * Prepares moving panes to another window (`TermSession.handOver`), by
   * pane id. Panes without a session aren't included. If one fails, the
   * others are resumed and this throws.
   */
  async handOver(paneIds: string[]): Promise<Record<string, PaneHandover>> {
    const live = paneIds.map((id) => this.sessions.get(id)).filter((s): s is TermSession => !!s && !s.disposed);
    const results = await Promise.allSettled(live.map((s) => s.handOver()));
    const failed = results.find((r) => r.status === "rejected");
    if (failed) {
      live.forEach((s) => s.resume());
      throw failed.reason;
    }
    return Object.fromEntries(live.map((s, i) => [s.paneId, (results[i] as PromiseFulfilledResult<PaneHandover>).value]));
  }

  /** The panes didn't move after all (see `handOver`). */
  resume(paneIds: string[]) {
    for (const id of paneIds) this.sessions.get(id)?.resume();
  }

  /** The panes moved to another window: drops their sessions, leaving the shells running. */
  release(paneIds: string[]) {
    for (const id of paneIds) {
      this.sessions.get(id)?.release();
      this.sessions.delete(id);
    }
  }

  /** Panes about to arrive from another window; their sessions are built from these. */
  expect(handovers: Record<string, PaneHandover>) {
    for (const [paneId, h] of Object.entries(handovers)) this.#incoming.set(paneId, h);
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
