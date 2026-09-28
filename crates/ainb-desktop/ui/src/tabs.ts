// Terminal tabs as the webview holds them, and the focus rules (base spec
// `:239-246`): while a terminal has focus only the shell accelerators stay
// with the shell, everything else goes to the pane, and Esc twice within
// 300 ms returns focus to the sidebar.

/** `ainb_desktop::terminal::TabTarget`. */
export type TabTarget = { kind: "session"; id: string; tmux: string } | { kind: "tmux"; tmux: string };

/** `ainb_desktop::terminal::TabView`. */
export type Tab = { key: string; target: TabTarget } & (
  | { state: "attached" }
  | { state: "reconnecting"; attempt: number }
  | { state: "detached" }
);

/** `ainb_desktop::terminal::TabsView`. */
export interface TabsView {
  tabs: Tab[];
  focus: string | null;
}

/** A session-list row, as `ainb_app::app::state::SessionListRowId` spells it. */
export type RowId = { session: string } | { other_tmux: string };

/** The session-list row a tab's target is. */
export function rowOf(target: TabTarget): RowId {
  return target.kind === "session" ? { session: target.id } : { other_tmux: target.tmux };
}

/**
 * What the window's `dispatch` command takes, generated from
 * `ainb_desktop::intent::RendererIntent` (#1158): a key, a named command with
 * its arguments, or pasted text. There is no pointer variant; the webview
 * hit-tests its own DOM and sends the command a press means.
 *
 * Re-exported here because this is where the window's intents are built; the
 * declaration itself is the Rust type's, checked fresh by CI.
 */
import type { RendererIntent } from "../../bindings/Desktop.ts";
export type { RendererIntent };

/**
 * The intent that selects `row` and attaches it. Opening goes through the
 * session list's own row, so the reducer marks the session attached each time.
 */
export function openRowIntent(row: RowId): RendererIntent {
  return { Command: ["session_list.select_row", { target: row, open: true }] };
}

/**
 * The intent that selects `row` without attaching it: for a row whose tab is
 * already open, so the sidebar and the answer banner follow the terminal
 * that is shown.
 */
export function selectRowIntent(row: RowId): RendererIntent {
  return { Command: ["session_list.select_row", { target: row, open: false }] };
}

/**
 * The select-only intent for tab `key`'s row, or `null` when no listed tab
 * has that key. Sent on every activation, a person's or the host's, so the
 * session list's selection is always the shown terminal's.
 */
export function selectIntentFor(tabs: readonly Tab[], key: string): RendererIntent | null {
  const tab = tabs.find((candidate) => candidate.key === key);
  return tab === undefined ? null : selectRowIntent(rowOf(tab.target));
}

/**
 * The session whose terminal the work area shows, the answer banner's scope
 * (`questionOver`): `undefined` when no terminal is shown, `null` for a tab
 * of no session (a bare tmux tab, or no active tab at all).
 */
export function shownSessionOf(
  showingTerminal: boolean,
  tabs: readonly Tab[],
  active: string | null,
): string | null | undefined {
  if (!showingTerminal) return undefined;
  const target = tabs.find((tab) => tab.key === active)?.target;
  return target?.kind === "session" ? target.id : null;
}

/** Automatic re-attaches before a tab offers "reattach": `REDIAL_DELAYS`. */
export const REDIALS = 3;

/** Esc twice within this long leaves the terminal. */
export const ESC_ESC_MS = 300;

/** What a shell accelerator asks for. */
export type Accelerator =
  | { kind: "tab"; index: number }
  | { kind: "prev" }
  | { kind: "next" }
  | { kind: "close" }
  | { kind: "palette" }
  | { kind: "new" }
  | { kind: "attention" }
  | { kind: "hosts" }
  | { kind: "copy" }
  | { kind: "paste" };

interface KeyLike {
  /** The physical key (`KeyW`, `Digit1`), so Shift does not change it. */
  code: string;
  metaKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
}

/**
 * The shell accelerator `event` is, or `null` for a key the pane gets. The
 * spec's `cmd` is Command on macOS; elsewhere, where Ctrl belongs to the pane
 * (ctrl+c, ctrl+b), it is Ctrl+Shift, as desktop terminals do, so there the
 * spec's cmd+shift+h is Ctrl+Shift+H.
 */
export function accelerator(event: KeyLike, mac: boolean): Accelerator | null {
  const mod = mac ? event.metaKey && !event.ctrlKey : event.ctrlKey && event.shiftKey && !event.metaKey;
  if (!mod || event.altKey) return null;
  if (event.code === "KeyH" && (!mac || event.shiftKey)) return { kind: "hosts" };
  if (mac && event.shiftKey) return null;
  // Copy and paste: macOS has them on the Edit menu, natively. Elsewhere the
  // pane owns ctrl+c and ctrl+v, so the shell's ctrl+shift pair does it.
  if (!mac && (event.code === "KeyC" || event.code === "KeyV")) {
    return { kind: event.code === "KeyC" ? "copy" : "paste" };
  }
  const digit = /^Digit([1-9])$/.exec(event.code);
  if (digit) return { kind: "tab", index: Number(digit[1]) - 1 };
  switch (event.code) {
    case "BracketLeft":
      return { kind: "prev" };
    case "BracketRight":
      return { kind: "next" };
    case "KeyW":
      return { kind: "close" };
    case "KeyK":
      return { kind: "palette" };
    // The new-worktree composer: Cmd+N on macOS, Ctrl+Shift+N elsewhere, like
    // every chord here. Plain Ctrl+N belongs to the pane (next-history in a
    // shell, completion in vim), so off macOS it needs the Shift.
    case "KeyN":
      return { kind: "new" };
    case "KeyU":
      return { kind: "attention" };
    default:
      return null;
  }
}

/**
 * Whether `shell` may run while a modal (the new-worktree composer) is open.
 * Only `new` may: every other chord switches, closes or focuses a tab, which
 * would act on the shell behind the modal and hand it the keyboard.
 */
export function acceleratorAllowedUnderModal(shell: Accelerator): boolean {
  return shell.kind === "new";
}

/** Whether an open modal refuses `shell`: the one gate the window's keydown
 * handler and every other path into the shell's chords (a terminal's own key
 * handler) both ask. */
export function modalBlocks(shell: Accelerator, modalOpen: boolean): boolean {
  return modalOpen && !acceleratorAllowedUnderModal(shell);
}

/**
 * The window's keydown handler: a shell chord runs, and its key is taken, so
 * nothing else also acts on it. A chord an open modal refuses is left alone
 * entirely, not taken and not run: it reaches the focused field, so under the
 * composer Ctrl+Shift+C and Ctrl+Shift+V still copy and paste off macOS.
 */
export function shellKeydown(deps: {
  mac: boolean;
  modalOpen(): boolean;
  run(shell: Accelerator): void;
}): (event: KeyboardEvent) => void {
  return (event) => {
    if (event.defaultPrevented) return;
    const shell = accelerator(event, deps.mac);
    if (shell === null || modalBlocks(shell, deps.modalOpen())) return;
    event.preventDefault();
    deps.run(shell);
  };
}

/** The shape of a focused element this needs: `document.activeElement` fits. */
interface FocusedLike {
  tagName: string;
  className: string;
}

/**
 * Whether `active`, the element with the keyboard, is a text field that is
 * not the terminal's own: the palette's query, the answer banner's composer,
 * the settings search.
 */
export function keyboardTaken(active: FocusedLike | null | undefined): boolean {
  if (!active) return false;
  const tag = active.tagName.toUpperCase();
  if (tag !== "INPUT" && tag !== "TEXTAREA") return false;
  return !active.className.split(/\s+/).includes("xterm-helper-textarea");
}

/** Who asked for a terminal to take the keyboard. */
export type FocusRequest = {
  /** The palette is open: it owns the keyboard until it closes. */
  palette: boolean;
  /** The new-worktree composer is open: a modal owns the keyboard too. */
  composer?: boolean;
  /**
   * The host asked, on its own schedule (a tab-open answer, the strip tidying
   * up), rather than a person pressing a tab chord or clicking a tab.
   */
  byHost: boolean;
  /** The element with the keyboard as the request lands. */
  active: FocusedLike | null | undefined;
};

/**
 * Whether a terminal asked for focus may take it. Never under the open
 * palette. A request of the host's stands down while a text field that is
 * not the terminal's own has the keyboard: the host's answer to a tab open
 * can land after the cursor was put in the answer banner's composer, and the
 * answer being typed would go to the agent's pane (#47). A person's own
 * chord or click on a tab is that person moving the keyboard, and it moves.
 */
export function terminalMayTakeFocus(request: FocusRequest): boolean {
  if (request.palette || request.composer) return false;
  return !(request.byHost && keyboardTaken(request.active));
}

/** A tracker that answers `true` for the second Esc within `ESC_ESC_MS`. */
export function escEsc(windowMs = ESC_ESC_MS): (now: number) => boolean {
  let last = -Infinity;
  return (now) => {
    const second = now - last <= windowMs;
    last = second ? -Infinity : now;
    return second;
  };
}

/** The key of the tab `step` places from `current`, wrapping. */
export function stepTab(tabs: readonly Tab[], current: string | null, step: number): string | null {
  if (tabs.length === 0) return null;
  const at = tabs.findIndex((tab) => tab.key === current);
  const from = at < 0 ? 0 : at;
  return tabs[(from + step + tabs.length) % tabs.length].key;
}
