// Lazy loading of xterm.js and its addons (kept out of the initial bundle),
// plus the terminal color themes derived from the app's design tokens.

import type { ITheme } from "@xterm/xterm";

export type XtermModules = Awaited<ReturnType<typeof importAll>>;

async function importAll() {
  const [xterm, fit, search, links, unicode] = await Promise.all([
    import("@xterm/xterm"),
    import("@xterm/addon-fit"),
    import("@xterm/addon-search"),
    import("@xterm/addon-web-links"),
    import("@xterm/addon-unicode11"),
    import("@xterm/xterm/css/xterm.css"),
  ]);
  return {
    Terminal: xterm.Terminal,
    FitAddon: fit.FitAddon,
    SearchAddon: search.SearchAddon,
    WebLinksAddon: links.WebLinksAddon,
    Unicode11Addon: unicode.Unicode11Addon,
  };
}

let modules: Promise<XtermModules> | undefined;

export function loadXterm(): Promise<XtermModules> {
  modules ??= importAll().catch((e) => {
    modules = undefined;
    throw e;
  });
  return modules;
}

let webglOk: boolean | undefined;

/** WebGL2 is what the webgl renderer needs; probe once. */
export function webglSupported(): boolean {
  if (webglOk === undefined) {
    try {
      const canvas = document.createElement("canvas");
      const gl = canvas.getContext("webgl2");
      webglOk = !!gl;
      gl?.getExtension("WEBGL_lose_context")?.loseContext();
    } catch {
      webglOk = false;
    }
  }
  return webglOk;
}

/** After a context loss we stay on the DOM renderer for the rest of the session. */
export function disableWebgl() {
  webglOk = false;
}

let webgl: Promise<typeof import("@xterm/addon-webgl")> | undefined;

/** The WebGL addon module, or null when WebGL isn't available (DOM renderer then). */
export async function loadWebgl(): Promise<typeof import("@xterm/addon-webgl").WebglAddon | null> {
  if (!webglSupported()) return null;
  try {
    webgl ??= import("@xterm/addon-webgl");
    return (await webgl).WebglAddon;
  } catch {
    return null;
  }
}

// ------------------------------------------------------------------ themes

/** ANSI palettes tuned for the app's light and dark backgrounds. */
const ANSI_LIGHT = {
  black: "#24292f",
  red: "#cf222e",
  green: "#116329",
  yellow: "#7d4e00",
  blue: "#0969da",
  magenta: "#8250df",
  cyan: "#1b7c83",
  white: "#6e7781",
  brightBlack: "#57606a",
  brightRed: "#a40e26",
  brightGreen: "#1a7f37",
  brightYellow: "#9a6700",
  brightBlue: "#218bff",
  brightMagenta: "#a475f9",
  brightCyan: "#3192aa",
  brightWhite: "#8c959f",
};

const ANSI_DARK = {
  black: "#3a3a40",
  red: "#f47174",
  green: "#5fd38d",
  yellow: "#f3c969",
  blue: "#6aa5ff",
  magenta: "#c49bff",
  cyan: "#4fd1e0",
  white: "#d4d4d8",
  brightBlack: "#71717a",
  brightRed: "#ff9b9d",
  brightGreen: "#8ae6ad",
  brightYellow: "#fbe08f",
  brightBlue: "#9cc3ff",
  brightMagenta: "#dcc2ff",
  brightCyan: "#8ce6f0",
  brightWhite: "#fafafa",
};

function token(style: CSSStyleDeclaration, name: string, fallback: string): string {
  return style.getPropertyValue(name).trim() || fallback;
}

/** Builds the xterm theme from the current CSS tokens (call after the theme switched). */
export function terminalTheme(dark: boolean): ITheme {
  const style = getComputedStyle(document.documentElement);
  const bg = token(style, "--bg", dark ? "#151517" : "#ffffff");
  const fg = token(style, "--text", dark ? "#ececef" : "#18181b");
  const accent = token(style, "--accent", dark ? "#3b7cf6" : "#2563eb");
  return {
    background: bg,
    foreground: fg,
    cursor: accent,
    cursorAccent: bg,
    selectionBackground: dark ? "#3b7cf666" : "#2563eb33",
    selectionInactiveBackground: dark ? "#3b7cf633" : "#2563eb1f",
    scrollbarSliderBackground: token(style, "--scrollbar", dark ? "rgba(255,255,255,0.14)" : "rgba(15,15,20,0.16)"),
    scrollbarSliderHoverBackground: token(style, "--scrollbar-hover", dark ? "rgba(255,255,255,0.26)" : "rgba(15,15,20,0.3)"),
    scrollbarSliderActiveBackground: token(style, "--scrollbar-hover", dark ? "rgba(255,255,255,0.26)" : "rgba(15,15,20,0.3)"),
    overviewRulerBorder: bg,
    ...(dark ? ANSI_DARK : ANSI_LIGHT),
  };
}

/** Search match highlight colors (need `allowProposedApi` decorations). */
export function searchDecorations(dark: boolean) {
  return {
    matchBackground: dark ? "#f3c96940" : "#f5c51866",
    matchBorder: "#00000000",
    matchOverviewRuler: dark ? "#f3c969" : "#d4a106",
    activeMatchBackground: dark ? "#f59e0b" : "#f59e0b",
    activeMatchBorder: dark ? "#fbbf24" : "#b45309",
    activeMatchColorOverviewRuler: dark ? "#f59e0b" : "#b45309",
  };
}
