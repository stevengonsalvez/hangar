// Which theme the window paints: the person's choice, or the system's.
//
// The choice is a per-viewer convenience kept in localStorage. Every read and
// write is wrapped: storage can be missing or throw (private window, blocked
// site data), and the window must still paint, in the system's theme.
//
// localStorage is the one source of the pick. The host keeps a copy, told on
// every change and once at start, only so the next launch's native window
// opens in the right theme before this page has painted (`theme.rs`).

import { createSignal, type Accessor } from "solid-js";
import type { ThemePreference } from "../../../bindings/Desktop.ts";

/** What a person can pick: the host's own type, so the two cannot drift. */
export type { ThemePreference };

/** What is actually painted. */
export type Theme = "dark" | "light";

/** The localStorage key holding the preference. */
export const THEME_KEY = "ainb.theme";

/** The smallest storage surface this module needs, so tests can pass a fake. */
export interface ThemeStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/** The theme to paint for `preference`, given whether the system prefers dark. */
export function resolveTheme(preference: ThemePreference, systemPrefersDark: boolean): Theme {
  if (preference === "system") return systemPrefersDark ? "dark" : "light";
  return preference;
}

/** The stored preference, or `system` when absent, unknown, or unreadable. */
export function readPreference(storage: ThemeStorage | undefined): ThemePreference {
  try {
    const value = storage?.getItem(THEME_KEY);
    return value === "dark" || value === "light" || value === "system" ? value : "system";
  } catch {
    return "system";
  }
}

/** Store `preference`; a storage that throws only loses the memory of it. */
export function writePreference(storage: ThemeStorage | undefined, preference: ThemePreference): void {
  try {
    storage?.setItem(THEME_KEY, preference);
  } catch {
    // Nothing to do: the theme still applies for this window's life.
  }
}

/** The classes on <html> for `theme`: exactly one of `dark` and `light`. */
export function applyTheme(root: { classList: DOMTokenList }, theme: Theme): void {
  root.classList.toggle("dark", theme === "dark");
  root.classList.toggle("light", theme === "light");
}

function safeStorage(): ThemeStorage | undefined {
  try {
    return window.localStorage;
  } catch {
    return undefined;
  }
}

/** The window's theme: the preference a person picked, and how to change it. */
export interface ThemeControl {
  /** The current preference, reactive: a settings control reads it. */
  preference: Accessor<ThemePreference>;
  /** The theme actually painted (the preference resolved against the
   * system's), reactive: the terminals follow it. */
  painted: Accessor<Theme>;
  /** Pick `next`: stored, and painted at once. */
  set(next: ThemePreference): void;
}

/**
 * Paint the stored preference now and follow the system while it is `system`.
 * Returns the control a settings page reads and sets; transitions are held
 * off for two frames around each switch so colours do not cross-fade at
 * different speeds.
 *
 * `tellHost` hears the stored preference once, now, and every pick after, so
 * the host's copy converges on this page's even when it was never told (a
 * pick made before the host kept one).
 */
export function startTheme(tellHost: (preference: ThemePreference) => void = () => {}): ThemeControl {
  const root = document.documentElement;
  const query = window.matchMedia?.("(prefers-color-scheme: dark)");
  const [preference, setPreference] = createSignal(readPreference(safeStorage()));
  tellHost(preference());
  const [painted, setPainted] = createSignal<Theme>(resolveTheme(preference(), query?.matches ?? true));
  const paint = () => {
    root.classList.add("theme-switching");
    const theme = resolveTheme(preference(), query?.matches ?? true);
    setPainted(theme);
    applyTheme(root, theme);
    requestAnimationFrame(() => requestAnimationFrame(() => root.classList.remove("theme-switching")));
  };
  query?.addEventListener?.("change", () => {
    if (preference() === "system") paint();
  });
  paint();
  return {
    preference,
    painted,
    set(next) {
      setPreference(next);
      writePreference(safeStorage(), next);
      tellHost(next);
      paint();
    },
  };
}

/** The xterm palette: the colours a terminal draws with. */
export interface TerminalColors {
  background: string;
  foreground: string;
  cursor: string;
  cursorAccent?: string;
  selectionBackground: string;
  selectionForeground?: string;
  black?: string;
  red?: string;
  green?: string;
  yellow?: string;
  blue?: string;
  magenta?: string;
  cyan?: string;
  white?: string;
  brightBlack?: string;
  brightRed?: string;
  brightGreen?: string;
  brightYellow?: string;
  brightBlue?: string;
  brightMagenta?: string;
  brightCyan?: string;
  brightWhite?: string;
}

/**
 * The terminal follows the window's theme, as Orca's does: Orca resolves its
 * terminal from the app theme and, by default, uses a separate light palette
 * in light mode (`resolveEffectiveTerminalAppearance`, and
 * `terminalUseSeparateLightTheme: true` in its default settings).
 *
 * Dark keeps this window's own cockpit terminal colours. Light is Orca's
 * default light palette, "Builtin Tango Light" (its `terminal-themes/defaults.ts`),
 * including the ANSI colours Orca darkened so an agent CLI's accent text stays
 * readable on white.
 */
export const TERMINAL_THEMES: Record<Theme, TerminalColors> = {
  dark: {
    background: "#0b0e14",
    foreground: "rgb(226, 232, 240)",
    cursor: "rgb(96, 165, 250)",
    selectionBackground: "#1e2636",
  },
  light: {
    background: "#ffffff",
    foreground: "#2e3434",
    cursor: "#2e3434",
    cursorAccent: "#ffffff",
    selectionBackground: "#accef7",
    selectionForeground: "#2e3434",
    black: "#2e3436",
    red: "#cc0000",
    green: "#4e9a06",
    yellow: "#8e7700",
    blue: "#3465a4",
    magenta: "#75507b",
    cyan: "#05727e",
    white: "#6a6a6a",
    brightBlack: "#555753",
    brightRed: "#ef2929",
    brightGreen: "#1b7a1b",
    brightYellow: "#6d5a00",
    brightBlue: "#204a87",
    brightMagenta: "#ad7fa8",
    brightCyan: "#034b50",
    brightWhite: "#3d3d3d",
  },
};

/**
 * The floor xterm holds text to against its cell, per painted theme: Orca's
 * `LIGHT_BG_MIN_CONTRAST` (4.5, WCAG AA) and `DARK_BG_MIN_CONTRAST` (3)
 * (`terminal-contrast-correction.ts`). Orca gates it by the background's
 * luminance; each of these themes has one fixed background, so the theme
 * decides it.
 *
 * Why a floor as well as the palette: an agent CLI in its own dark theme
 * paints in 24-bit colour, which no palette slot reaches. Claude Code's dark
 * theme sends bold headings as white and prompt lines on an rgb(55, 55, 55)
 * bar in the default foreground; on the light background those read as
 * nothing and as solid black bars. xterm lifts such text to the floor and
 * leaves colours that already clear it as sent.
 */
export const TERMINAL_MIN_CONTRAST: Record<Theme, number> = { dark: 3, light: 4.5 };

/** The xterm options that follow the painted theme: its palette and its floor. */
export function terminalAppearance(theme: Theme): { theme: TerminalColors; minimumContrastRatio: number } {
  return { theme: TERMINAL_THEMES[theme], minimumContrastRatio: TERMINAL_MIN_CONTRAST[theme] };
}
