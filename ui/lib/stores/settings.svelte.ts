// User preferences persisted in localStorage.

export type ThemePref = "system" | "light" | "dark";
export type CursorStyle = "block" | "bar" | "underline";
export type BellStyle = "visual" | "none";

const KEY = "patchbay.settings.v1";

export interface TerminalPrefs {
  /** CSS font-family list; "" = the default monospace stack. */
  fontFamily: string;
  fontSize: number;
  lineHeight: number;
  cursorStyle: CursorStyle;
  cursorBlink: boolean;
  scrollback: number;
  copyOnSelect: boolean;
  bell: BellStyle;
  /** Ask before pasting several lines into a shell without bracketed paste. */
  confirmMultilinePaste: boolean;
}

/** Cross-platform monospace stack: macOS, Windows, common Linux fonts, then generic. */
export const DEFAULT_TERMINAL_FONT =
  '"JetBrains Mono", "SF Mono", Menlo, "Cascadia Mono", Consolas, "DejaVu Sans Mono", "Ubuntu Mono", "Liberation Mono", "Noto Sans Mono", monospace';

export const TERMINAL_LIMITS = {
  fontSize: [8, 32],
  lineHeight: [1, 2],
  scrollback: [0, 200_000],
} as const;

export const TERMINAL_DEFAULTS: TerminalPrefs = {
  fontFamily: "",
  fontSize: 13,
  lineHeight: 1.2,
  cursorStyle: "block",
  cursorBlink: true,
  scrollback: 10_000,
  copyOnSelect: false,
  bell: "visual",
  confirmMultilinePaste: true,
};

/** When restored remote tabs connect. */
export type ReconnectMode = "lazy" | "eager";

export interface SessionPrefs {
  /** Bring back the previous tabs, split and terminal panes on launch. */
  restoreTabs: boolean;
  /** lazy = when a restored tab is first shown; eager = all at launch. */
  reconnect: ReconnectMode;
}

export const SESSION_DEFAULTS: SessionPrefs = { restoreTabs: true, reconnect: "lazy" };

interface Persisted {
  theme: ThemePref;
  sidebarWidth: number;
  collapsedGroups: string[];
  terminal: TerminalPrefs;
  session: SessionPrefs;
}

const DEFAULTS: Persisted = {
  theme: "system",
  sidebarWidth: 260,
  collapsedGroups: [],
  terminal: { ...TERMINAL_DEFAULTS },
  session: { ...SESSION_DEFAULTS },
};

function loadSession(raw: unknown): SessionPrefs {
  const s = (raw && typeof raw === "object" ? raw : {}) as Partial<Record<keyof SessionPrefs, unknown>>;
  return {
    restoreTabs: typeof s.restoreTabs === "boolean" ? s.restoreTabs : SESSION_DEFAULTS.restoreTabs,
    reconnect: s.reconnect === "eager" ? "eager" : "lazy",
  };
}

const clamp = (v: unknown, [lo, hi]: readonly [number, number], fallback: number) =>
  typeof v === "number" && Number.isFinite(v) ? Math.min(hi, Math.max(lo, v)) : fallback;

function loadTerminal(raw: unknown): TerminalPrefs {
  const t = (raw && typeof raw === "object" ? raw : {}) as Partial<Record<keyof TerminalPrefs, unknown>>;
  const d = TERMINAL_DEFAULTS;
  return {
    fontFamily: typeof t.fontFamily === "string" ? t.fontFamily : d.fontFamily,
    fontSize: clamp(t.fontSize, TERMINAL_LIMITS.fontSize, d.fontSize),
    lineHeight: clamp(t.lineHeight, TERMINAL_LIMITS.lineHeight, d.lineHeight),
    cursorStyle: t.cursorStyle === "bar" || t.cursorStyle === "underline" ? t.cursorStyle : "block",
    cursorBlink: typeof t.cursorBlink === "boolean" ? t.cursorBlink : d.cursorBlink,
    scrollback: Math.round(clamp(t.scrollback, TERMINAL_LIMITS.scrollback, d.scrollback)),
    copyOnSelect: typeof t.copyOnSelect === "boolean" ? t.copyOnSelect : d.copyOnSelect,
    bell: t.bell === "none" ? "none" : "visual",
    confirmMultilinePaste: typeof t.confirmMultilinePaste === "boolean" ? t.confirmMultilinePaste : d.confirmMultilinePaste,
  };
}

function load(): Persisted {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return structuredClone(DEFAULTS);
    const p = JSON.parse(raw) as Partial<Persisted>;
    return {
      theme: p.theme === "light" || p.theme === "dark" ? p.theme : "system",
      sidebarWidth: typeof p.sidebarWidth === "number" ? p.sidebarWidth : DEFAULTS.sidebarWidth,
      collapsedGroups: Array.isArray(p.collapsedGroups) ? p.collapsedGroups.map(String) : [],
      terminal: loadTerminal(p.terminal),
      session: loadSession(p.session),
    };
  } catch {
    return structuredClone(DEFAULTS);
  }
}

export const SIDEBAR_MIN = 200;
export const SIDEBAR_MAX = 420;

const darkQuery =
  typeof window !== "undefined" ? window.matchMedia("(prefers-color-scheme: dark)") : null;

class Settings {
  #initial = load();
  theme = $state<ThemePref>(this.#initial.theme);
  sidebarWidth = $state(this.#initial.sidebarWidth);
  collapsedGroups = $state<string[]>(this.#initial.collapsedGroups);
  terminal = $state<TerminalPrefs>(this.#initial.terminal);
  session = $state<SessionPrefs>(this.#initial.session);
  systemDark = $state(darkQuery?.matches ?? false);
  resolvedTheme = $derived<"light" | "dark">(
    this.theme === "system" ? (this.systemDark ? "dark" : "light") : this.theme,
  );

  constructor() {
    darkQuery?.addEventListener("change", (e) => {
      this.systemDark = e.matches;
      this.#apply();
    });
    this.#apply();
  }

  setTheme(theme: ThemePref) {
    this.theme = theme;
    this.#apply();
    this.#save();
  }

  setSidebarWidth(px: number, persist = true) {
    this.sidebarWidth = Math.round(Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, px)));
    if (persist) this.#save();
  }

  /** Updates terminal preferences (validated) and persists them. */
  setTerminal(patch: Partial<TerminalPrefs>) {
    this.terminal = loadTerminal({ ...$state.snapshot(this.terminal), ...patch });
    this.#save();
  }

  setSession(patch: Partial<SessionPrefs>) {
    this.session = loadSession({ ...$state.snapshot(this.session), ...patch });
    this.#save();
  }

  resetTerminal() {
    this.terminal = { ...TERMINAL_DEFAULTS };
    this.#save();
  }

  isCollapsed(group: string): boolean {
    return this.collapsedGroups.includes(group);
  }

  toggleGroup(group: string, collapsed = !this.isCollapsed(group)) {
    this.collapsedGroups = collapsed
      ? [...new Set([...this.collapsedGroups, group])]
      : this.collapsedGroups.filter((g) => g !== group);
    this.#save();
  }

  #apply() {
    if (typeof document === "undefined") return;
    const resolved = this.theme === "system" ? (this.systemDark ? "dark" : "light") : this.theme;
    document.documentElement.dataset.theme = resolved;
  }

  #save() {
    try {
      const data: Persisted = {
        theme: this.theme,
        sidebarWidth: this.sidebarWidth,
        collapsedGroups: this.collapsedGroups,
        terminal: this.terminal,
        session: this.session,
      };
      localStorage.setItem(KEY, JSON.stringify(data));
    } catch {
      // Storage unavailable (private mode etc.): settings just won't persist.
    }
  }
}

export const settings = new Settings();
