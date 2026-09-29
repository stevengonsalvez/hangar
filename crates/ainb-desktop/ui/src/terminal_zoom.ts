// Terminal zoom, as Orca's panes do it (`useTerminalFontZoom.ts`): the chord
// steps the focused pane's font by a point between 8 and 32, reset goes back
// to the base size, and the size lives with that pane only, never saved.

/** The keys a zoom chord needs: `KeyboardEvent` fits. */
interface KeyLike {
  /** The physical key (`Equal`, `Digit0`), so Shift does not change it. */
  code: string;
  metaKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
}

export type ZoomDirection = "in" | "out" | "reset";

/** A pane's font size before any zoom, and what reset returns to. */
export const TERMINAL_FONT_SIZE = 13;
export const MIN_FONT_SIZE = 8;
export const MAX_FONT_SIZE = 32;
const FONT_SIZE_STEP = 1;

/**
 * The zoom `event` asks for, or `null` for a key the pane gets. Orca binds
 * `Mod+Equal`, `Mod+Shift+Plus` and `Mod+NumpadAdd` in, `Mod+Minus` and
 * `Mod+NumpadSubtract` out, `Mod+0` reset, with Mod Command on macOS and Ctrl
 * elsewhere. Unlike the shell chords in `tabs.ts`, plain Ctrl is right here:
 * xterm sends the pane no byte for Ctrl+=, Ctrl+- or Ctrl+0, while
 * Ctrl+Shift+- is Ctrl+_, readline's undo, which a Ctrl+Shift form would take.
 */
export function zoomChord(event: KeyLike, mac: boolean): ZoomDirection | null {
  if (event.altKey) return null;
  if (mac ? !event.metaKey || event.ctrlKey : !event.ctrlKey || event.metaKey) return null;
  // Only zoom in has a Shift form (Cmd++ is Cmd+Shift+=).
  const shifted = event.shiftKey;
  switch (event.code) {
    case "Equal":
    case "NumpadAdd":
      return "in";
    case "Minus":
    case "NumpadSubtract":
      return shifted ? null : "out";
    case "Digit0":
      return shifted ? null : "reset";
    default:
      return null;
  }
}

/** The font size one `direction` press moves a pane at `current` to. */
export function nextFontSize(current: number, direction: ZoomDirection): number {
  if (direction === "reset") return TERMINAL_FONT_SIZE;
  const next = direction === "in" ? current + FONT_SIZE_STEP : current - FONT_SIZE_STEP;
  return Math.max(MIN_FONT_SIZE, Math.min(MAX_FONT_SIZE, next));
}
