// The terminal focus rules: which keys stay with the shell, Esc Esc, tab steps.

import assert from "node:assert/strict";
import { test } from "node:test";
import {
  accelerator,
  escEsc,
  keyboardTaken,
  leftToFocus,
  openRowIntent,
  rowOf,
  selectIntentFor,
  selectRowIntent,
  shownSessionOf,
  stepTab,
  tabAfterClose,
  acceleratorAllowedUnderModal,
  modalBlocks,
  type Accelerator,
  terminalMayTakeFocus,
  type Tab,
  visited,
} from "./tabs.ts";

const key = (code: string, mods: Partial<{ meta: boolean; ctrl: boolean; shift: boolean; alt: boolean }> = {}) => ({
  code,
  metaKey: mods.meta ?? false,
  ctrlKey: mods.ctrl ?? false,
  shiftKey: mods.shift ?? false,
  altKey: mods.alt ?? false,
});

test("on macOS only cmd chords stay with the shell", () => {
  assert.deepEqual(accelerator(key("Digit3", { meta: true }), true), { kind: "tab", index: 2 });
  assert.deepEqual(accelerator(key("KeyW", { meta: true }), true), { kind: "close" });
  assert.deepEqual(accelerator(key("BracketLeft", { meta: true }), true), { kind: "prev" });
  assert.deepEqual(accelerator(key("KeyK", { meta: true }), true), { kind: "clear" });
  assert.deepEqual(accelerator(key("KeyJ", { meta: true }), true), { kind: "palette" });
  assert.deepEqual(accelerator(key("KeyH", { meta: true, shift: true }), true), { kind: "hosts" });
  // The pane keeps ctrl+c, ctrl+b, arrows, and cmd chords the spec does not list.
  assert.equal(accelerator(key("KeyC", { ctrl: true }), true), null);
  assert.equal(accelerator(key("KeyB", { ctrl: true }), true), null);
  assert.equal(accelerator(key("ArrowUp"), true), null);
  assert.equal(accelerator(key("KeyH", { meta: true }), true), null);
  assert.equal(accelerator(key("KeyW", { meta: true, shift: true }), true), null);
});

test("elsewhere the shell's mod is ctrl+shift and plain ctrl reaches the pane", () => {
  assert.deepEqual(accelerator(key("Digit1", { ctrl: true, shift: true }), false), { kind: "tab", index: 0 });
  assert.deepEqual(accelerator(key("BracketRight", { ctrl: true, shift: true }), false), { kind: "next" });
  assert.deepEqual(accelerator(key("KeyH", { ctrl: true, shift: true }), false), { kind: "hosts" });
  assert.equal(accelerator(key("KeyW", { ctrl: true }), false), null);
  assert.equal(accelerator(key("KeyC", { ctrl: true }), false), null);
  assert.equal(accelerator(key("Digit1", { meta: true }), false), null);
});

test("the composer opens on Cmd+N on macOS and Ctrl+Shift+N elsewhere", () => {
  assert.deepEqual(accelerator(key("KeyN", { meta: true }), true), { kind: "new" });
  assert.deepEqual(accelerator(key("KeyN", { ctrl: true, shift: true }), false), { kind: "new" });

  // Plain Ctrl+N is the pane's (next-history, vim completion): never taken.
  assert.equal(accelerator(key("KeyN", { ctrl: true }), false), null);
  // Shift on macOS, Alt, or the other platform's modifier: not the chord.
  assert.equal(accelerator(key("KeyN", { meta: true, shift: true }), true), null);
  assert.equal(accelerator(key("KeyN", { meta: true, alt: true }), true), null);
  assert.equal(accelerator(key("KeyN", { ctrl: true }), true), null);
  assert.equal(accelerator(key("KeyN", { meta: true }), false), null);
  assert.equal(accelerator(key("KeyN"), true), null);
});

test("Orca's chords: Cmd+K clears, Cmd+J is the palette, Cmd+B the sidebar; Ctrl+Shift off macOS", () => {
  for (const [code, shell] of [
    ["KeyK", { kind: "clear" }],
    ["KeyJ", { kind: "palette" }],
    ["KeyB", { kind: "sidebar" }],
  ] as const) {
    assert.deepEqual(accelerator(key(code, { meta: true }), true), shell);
    assert.deepEqual(accelerator(key(code, { ctrl: true, shift: true }), false), shell);
    // Plain Ctrl is the pane's: Ctrl+K kill-line, Ctrl+J newline, Ctrl+B tmux's prefix.
    assert.equal(accelerator(key(code, { ctrl: true }), false), null);
    assert.equal(accelerator(key(code, { ctrl: true }), true), null);
    assert.equal(accelerator(key(code, { meta: true, shift: true }), true), null);
  }
});

test("worktree steps are Cmd+Shift+Up and Down on macOS, and the pane's keys elsewhere", () => {
  assert.deepEqual(accelerator(key("ArrowUp", { meta: true, shift: true }), true), { kind: "worktree", step: -1 });
  assert.deepEqual(accelerator(key("ArrowDown", { meta: true, shift: true }), true), { kind: "worktree", step: 1 });
  assert.equal(accelerator(key("ArrowUp", { meta: true }), true), null);
  assert.equal(accelerator(key("ArrowLeft", { meta: true, shift: true }), true), null);
  // xterm sends Ctrl+Shift+Up as ESC [1;6A: the pane keeps it.
  assert.equal(accelerator(key("ArrowUp", { ctrl: true, shift: true }), false), null);
  assert.equal(accelerator(key("ArrowDown", { ctrl: true, shift: true }), false), null);
});

test("clear is always the focused pane's; a worktree step is a text field's own when one has the keyboard", () => {
  const field = { tagName: "INPUT", className: "settings-search" };
  const pane = { tagName: "TEXTAREA", className: "xterm-helper-textarea" };
  assert.equal(leftToFocus({ kind: "clear" }, null), true);
  assert.equal(leftToFocus({ kind: "clear" }, pane), true);
  assert.equal(leftToFocus({ kind: "worktree", step: 1 }, field), true);
  assert.equal(leftToFocus({ kind: "worktree", step: 1 }, pane), false);
  assert.equal(leftToFocus({ kind: "worktree", step: -1 }, null), false);
  assert.equal(leftToFocus({ kind: "palette" }, field), false, "every other chord is the window's");
});

test("a new terminal opens on Cmd+T on macOS and Ctrl+Shift+T elsewhere", () => {
  assert.deepEqual(accelerator(key("KeyT", { meta: true }), true), { kind: "terminal" });
  assert.deepEqual(accelerator(key("KeyT", { ctrl: true, shift: true }), false), { kind: "terminal" });
  // Plain Ctrl+T is the pane's (transpose in a shell): never taken.
  assert.equal(accelerator(key("KeyT", { ctrl: true }), false), null);
  // Cmd+Shift+T is reopen closed tab, as Orca's (`recent_tabs.ts`).
  assert.deepEqual(accelerator(key("KeyT", { meta: true, shift: true }), true), { kind: "reopen" });
  assert.equal(accelerator(key("KeyT"), true), null);
});

test("a shell tab has no row: activating it selects nothing", () => {
  const tabs = [{ key: "ainb-dsh-0123abcd", target: { kind: "shell", tmux: "ainb-dsh-0123abcd", dir: "/w/app" }, state: "attached" }] as Tab[];
  assert.equal(selectIntentFor(tabs, "ainb-dsh-0123abcd"), null);
});

test("copy and paste are the shell's only elsewhere, and native on macOS", () => {
  assert.deepEqual(accelerator(key("KeyC", { ctrl: true, shift: true }), false), { kind: "copy" });
  assert.deepEqual(accelerator(key("KeyV", { ctrl: true, shift: true }), false), { kind: "paste" });
  // The pane keeps plain ctrl+c and ctrl+v.
  assert.equal(accelerator(key("KeyC", { ctrl: true }), false), null);
  assert.equal(accelerator(key("KeyV", { ctrl: true }), false), null);
  // macOS: the Edit menu copies and pastes, so the shell claims neither.
  assert.equal(accelerator(key("KeyC", { meta: true }), true), null);
  assert.equal(accelerator(key("KeyV", { meta: true }), true), null);
});

test("the split chords are Orca's: Cmd+D and Cmd+Shift+D, Ctrl+Shift+D and Alt+Shift+D elsewhere", () => {
  // orca:src/shared/keybindings/definitions-core-4.ts:22-42
  assert.deepEqual(accelerator(key("KeyD", { meta: true }), true), { kind: "split", direction: "right" });
  assert.deepEqual(accelerator(key("KeyD", { meta: true, shift: true }), true), { kind: "split", direction: "down" });
  assert.deepEqual(accelerator(key("KeyD", { ctrl: true, shift: true }), false), { kind: "split", direction: "right" });
  assert.deepEqual(accelerator(key("KeyD", { alt: true, shift: true }), false), { kind: "split", direction: "down" });
  // The pane keeps the rest: ctrl+d is end-of-file, alt+d kills a word.
  // Alt+Shift+D off macOS is Orca's split down, and is taken from the pane
  // as Orca takes it.
  assert.equal(accelerator(key("KeyD", { ctrl: true }), false), null);
  assert.equal(accelerator(key("KeyD", { ctrl: true }), true), null);
  assert.equal(accelerator(key("KeyD", { alt: true }), false), null);
  assert.equal(accelerator(key("KeyD", { alt: true, shift: true }), true), null);
  assert.equal(accelerator(key("KeyD", { meta: true, alt: true }), true), null);
});

test("a split chord is refused under the composer, like every chord but new", () => {
  assert.equal(modalBlocks({ kind: "split", direction: "right" }, true), true);
  assert.equal(modalBlocks({ kind: "split", direction: "down" }, false), false);
});

test("Esc twice within 300 ms leaves the terminal; a slower pair does not", () => {
  const esc = escEsc();
  assert.equal(esc(1000), false);
  assert.equal(esc(1250), true);
  assert.equal(esc(1300), false, "a third Esc starts a new pair");
  assert.equal(esc(1700), false);
});

test("tab steps wrap in both directions", () => {
  const tabs = ["a", "b", "c"].map((k) => ({ key: k, target: { kind: "tmux", tmux: k }, state: "attached" }) as Tab);
  assert.equal(stepTab(tabs, "c", 1), "a");
  assert.equal(stepTab(tabs, "a", -1), "c");
  assert.equal(stepTab([], null, 1), null);
});

test("a tab reopens through its session-list row", () => {
  assert.deepEqual(rowOf({ kind: "session", id: "u-1", tmux: "t" }), { session: "u-1" });
  assert.deepEqual(rowOf({ kind: "tmux", tmux: "other" }), { other_tmux: "other" });
  assert.deepEqual(openRowIntent({ session: "u-1" }), {
    Command: ["session_list.select_row", { target: { session: "u-1" }, open: true }],
  });
  assert.deepEqual(selectRowIntent({ session: "u-1" }), {
    Command: ["session_list.select_row", { target: { session: "u-1" }, open: false }],
  });
});

test("a terminal stands down while a text field that is not its own has the keyboard", () => {
  assert.equal(keyboardTaken({ tagName: "INPUT", className: "palette-query" }), true);
  assert.equal(keyboardTaken({ tagName: "input", className: "" }), true);
  assert.equal(keyboardTaken({ tagName: "TEXTAREA", className: "some other" }), true);
  // The terminal's own textarea is where it wants to be.
  assert.equal(keyboardTaken({ tagName: "TEXTAREA", className: "xterm-helper-textarea" }), false);
  // A button, the body, or nothing focused: the terminal may take it.
  assert.equal(keyboardTaken({ tagName: "BUTTON", className: "session-row" }), false);
  assert.equal(keyboardTaken({ tagName: "BODY", className: "" }), false);
  assert.equal(keyboardTaken(null), false);
});

test("a terminal takes focus for a person, and for the host only when no text field has it", () => {
  const composer = { tagName: "INPUT", className: "" };
  const row = { tagName: "BUTTON", className: "session-row" };
  // The open palette owns the keyboard whoever asks.
  assert.equal(terminalMayTakeFocus({ palette: true, byHost: false, active: row }), false);
  assert.equal(terminalMayTakeFocus({ palette: true, byHost: true, active: row }), false);
  // So does the open composer: a modal, for a person's chord as much as the host.
  assert.equal(terminalMayTakeFocus({ palette: false, composer: true, byHost: false, active: row }), false);
  assert.equal(terminalMayTakeFocus({ palette: false, composer: true, byHost: true, active: null }), false);
  // The host's answer stands down for a text field; a person's chord does not.
  assert.equal(terminalMayTakeFocus({ palette: false, byHost: true, active: composer }), false);
  assert.equal(terminalMayTakeFocus({ palette: false, byHost: false, active: composer }), true);
  // Nothing else in the way: both take it.
  assert.equal(terminalMayTakeFocus({ palette: false, byHost: true, active: row }), true);
  assert.equal(terminalMayTakeFocus({ palette: false, byHost: false, active: null }), true);
});

test("under the open composer no chord reaches the shell but new", () => {
  const chords: Accelerator[] = [
    { kind: "tab", index: 0 },
    { kind: "prev" },
    { kind: "next" },
    { kind: "close" },
    { kind: "palette" },
    { kind: "terminal" },
    { kind: "attention" },
    { kind: "hosts" },
    { kind: "copy" },
    { kind: "paste" },
  ];
  for (const chord of chords) assert.equal(acceleratorAllowedUnderModal(chord), false, chord.kind);
  assert.equal(acceleratorAllowedUnderModal({ kind: "new" }), true);
});

test("the shown session is the active tab's, only while a terminal is shown", () => {
  const tabs: Tab[] = [
    { key: "k-1", target: { kind: "session", id: "u-1", tmux: "t1" }, state: "attached" },
    { key: "k-2", target: { kind: "tmux", tmux: "bare" }, state: "attached" },
  ] as Tab[];
  assert.equal(shownSessionOf(false, tabs, "k-1"), undefined, "the board: no terminal shown");
  assert.equal(shownSessionOf(true, tabs, "k-1"), "u-1");
  assert.equal(shownSessionOf(true, tabs, "k-2"), null, "a bare tmux tab has no session");
  assert.equal(shownSessionOf(true, tabs, null), null, "no active tab");
  assert.deepEqual(selectIntentFor(tabs, "k-1"), selectRowIntent({ session: "u-1" }));
  assert.deepEqual(selectIntentFor(tabs, "k-2"), selectRowIntent({ other_tmux: "bare" }));
  assert.equal(selectIntentFor(tabs, "gone"), null);
});

const strip = (...keys: string[]) =>
  keys.map((k) => ({ key: k, target: { kind: "tmux", tmux: k }, state: "attached" }) as Tab);

test("closing the shown tab shows the one shown before it, as Orca does", () => {
  // a b c open, a then c shown: closing c goes back to a, not to its neighbour b
  // and not to the strip's first tab by accident.
  const recent = visited(visited([], "a"), "c");
  assert.equal(tabAfterClose(strip("a", "b", "c"), strip("a", "b"), recent, "c"), "a");
  // The most recent other tab wins, wherever it sits on the strip.
  const back = visited(visited(visited([], "c"), "b"), "a");
  assert.equal(tabAfterClose(strip("a", "b", "c"), strip("b", "c"), back, "a"), "b");
});

test("the tab shown before wins over the strip's first and both neighbours", () => {
  // a b c d e open, b then d shown: closing d goes back to b, which is none of
  // a (the strip's first), c (its left) or e (its right).
  const recent = visited(visited([], "b"), "d");
  assert.equal(tabAfterClose(strip("a", "b", "c", "d", "e"), strip("a", "b", "c", "e"), recent, "d"), "b");
});

test("with no other tab shown yet, closing one shows its right neighbour, else its left", () => {
  assert.equal(tabAfterClose(strip("a", "b", "c"), strip("a", "c"), ["b"], "b"), "c");
  assert.equal(tabAfterClose(strip("a", "b", "c"), strip("a", "b"), ["c"], "c"), "b");
  assert.equal(tabAfterClose(strip("a"), strip(), ["a"], "a"), null);
});

test("a tab shown before but closed since is never picked", () => {
  const recent = visited(visited(visited([], "a"), "b"), "c");
  assert.equal(tabAfterClose(strip("a", "b", "c"), strip("a"), recent, "c"), "a");
});

test("with no shown tab to close, the strip's first tab is shown", () => {
  assert.equal(tabAfterClose(strip(), strip("x", "y"), [], null), "x");
});

test("a tab shown again moves to the most recent end, once", () => {
  assert.deepEqual(visited(["a", "b", "c"], "a"), ["b", "c", "a"]);
});

