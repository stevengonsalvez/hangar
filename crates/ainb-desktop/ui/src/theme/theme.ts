// Which theme the window paints: the person's choice, or the system's.
//
// The choice is a per-viewer convenience kept in localStorage. Every read and
// write is wrapped: storage can be missing or throw (private window, blocked
// site data), and the window must still paint, in the system's theme.

/** What a person can pick. */
export type ThemePreference = "system" | "dark" | "light";

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

/**
 * Paint the stored preference now and follow the system while it is `system`.
 * Returns a setter for a settings control; transitions are held off for two
 * frames around each switch so colours do not cross-fade at different speeds.
 */
export function startTheme(): (preference: ThemePreference) => void {
  const root = document.documentElement;
  const query = window.matchMedia?.("(prefers-color-scheme: dark)");
  let preference = readPreference(safeStorage());
  const paint = () => {
    root.classList.add("theme-switching");
    applyTheme(root, resolveTheme(preference, query?.matches ?? true));
    requestAnimationFrame(() => requestAnimationFrame(() => root.classList.remove("theme-switching")));
  };
  query?.addEventListener?.("change", () => {
    if (preference === "system") paint();
  });
  paint();
  return (next) => {
    preference = next;
    writePreference(safeStorage(), next);
    paint();
  };
}

/** The xterm colours for the painted theme, read from the tokens. */
export interface TerminalColors {
  background: string;
  foreground: string;
  cursor: string;
  selectionBackground: string;
}

/**
 * The terminal's colours, from the token values `read` returns (the computed
 * style of <html>). A token that reads empty (no stylesheet yet) falls back to
 * the dark palette, so a pane never paints unreadable text.
 */
export function terminalColors(read: (token: string) => string): TerminalColors {
  const pick = (token: string, fallback: string) => read(token).trim() || fallback;
  return {
    background: pick("--background", "#0b0e14"),
    foreground: pick("--foreground", "rgb(226, 232, 240)"),
    cursor: pick("--primary", "rgb(96, 165, 250)"),
    selectionBackground: pick("--accent", "#1e2636"),
  };
}
