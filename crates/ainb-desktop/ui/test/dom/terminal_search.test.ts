// A terminal's find bar, driven as a person drives it: the find chord pressed
// in a mounted pane (`TerminalView`, as the window mounts it), a query typed,
// Enter and Shift+Enter to walk the matches, Esc to close. The pane's output
// arrives through the channel the host writes to, so the matches are real
// ones xterm found in its own buffer.

import { hostCalls, settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent } from "solid-js";
import { render } from "solid-js/web";
import { accelerator } from "../../src/tabs.ts";
import { TerminalView } from "../../src/terminal.tsx";
import type { ByteChannel } from "../../src/transport.ts";

// The pane watches its host's size; happy-dom has the observer, Node does not.
Object.defineProperty(globalThis, "ResizeObserver", {
  value: (window as unknown as { ResizeObserver: unknown }).ResizeObserver,
  configurable: true,
  writable: true,
});

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  hostCalls.clear();
  document.body.innerHTML = "";
});

/** Let xterm paint and the search addon report: both run on timers. */
const paint = () => new Promise((done) => setTimeout(done, 50));

type Chord = { code: string; key: string; metaKey?: boolean; ctrlKey?: boolean; shiftKey?: boolean };

/** The class of the element with the keyboard: a string, so a failed
 * assertion prints it rather than walking a whole happy-dom node. */
const focused = () => document.activeElement?.className ?? "nothing";

const CMD_F: Chord = { code: "KeyF", key: "f", metaKey: true };
const CTRL_SHIFT_F: Chord = { code: "KeyF", key: "F", ctrlKey: true, shiftKey: true };
const CTRL_F: Chord = { code: "KeyF", key: "f", ctrlKey: true };

function press(target: Element, chord: Chord): KeyboardEvent {
  const event = new window.KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...chord });
  target.dispatchEvent(event);
  return event as unknown as KeyboardEvent;
}

/** One attached pane on `mac` or not, with `text` painted into it. */
async function mountPane(mac: boolean, text: string) {
  const container = document.createElement("div");
  document.body.appendChild(container);
  cleanup = render(
    () =>
      createComponent(TerminalView, {
        tab: { key: "tmux_app", target: { kind: "session", id: "s-1", tmux: "tmux_app" }, state: "attached" } as never,
        title: "app",
        active: true,
        mac,
        onAccelerator() {},
        onLeave() {},
        focusRef() {},
        theme: "dark",
      }),
    container,
  );
  await settle();
  const output = (hostCalls.get("terminal_output") as { bytes: ByteChannel } | undefined)?.bytes;
  assert.ok(output, "the pane opened its output channel");
  output.onmessage(new TextEncoder().encode(text).buffer as ArrayBuffer);
  await paint();
  const terminal = container.querySelector(".xterm-helper-textarea") as HTMLTextAreaElement;
  assert.ok(terminal, "xterm mounted its keyboard target");
  terminal.focus();
  const bar = () => container.querySelector(".terminal-search");
  const query = () => container.querySelector(".terminal-search-query") as HTMLInputElement | null;
  const count = () => container.querySelector(".terminal-search-count")?.textContent;
  const type = async (value: string) => {
    const field = query();
    assert.ok(field, "the query field is there to type in");
    field.value = value;
    field.dispatchEvent(new window.Event("input", { bubbles: true }));
    await paint();
  };
  const key = async (chord: Chord) => {
    press(document.activeElement ?? terminal, chord);
    await paint();
  };
  return { container, terminal, bar, query, count, type, key };
}

const OUTPUT = "alpha needle one\r\nbeta Needle two\r\ngamma needle three\r\n";

test("Cmd+F on macOS opens the bar over the focused pane, the query field focused", async () => {
  const pane = await mountPane(true, OUTPUT);
  assert.equal(pane.bar(), null, "no bar before the chord");
  const event = press(pane.terminal, CMD_F);
  await settle();
  assert.ok(pane.bar(), "the chord opened the bar");
  assert.equal(event.defaultPrevented, true, "and the pane did not also take the key");
  assert.equal(focused(), "terminal-search-query", "the query field has the keyboard");
  assert.equal(pane.count(), "0/0");
});

test("off macOS Ctrl+Shift+F opens the bar, and plain Ctrl+F stays the pane's", async () => {
  const pane = await mountPane(false, OUTPUT);
  press(pane.terminal, CTRL_F);
  await settle();
  assert.equal(pane.bar(), null, "Ctrl+F is forward-char in the shell, not find");
  press(pane.terminal, CTRL_SHIFT_F);
  await settle();
  assert.ok(pane.bar(), "Ctrl+Shift+F opened the bar");
  assert.equal(focused(), "terminal-search-query");
});

test("the find chord is no shell accelerator, so it steals none of the window's", () => {
  assert.equal(accelerator({ ...CMD_F, ctrlKey: false, shiftKey: false, altKey: false }, true), null);
  assert.equal(accelerator({ ...CTRL_SHIFT_F, metaKey: false, altKey: false }, false), null);
});

test("typing finds the matches in the pane's buffer, case-blind until Aa is pressed", async () => {
  const pane = await mountPane(true, OUTPUT);
  await pane.key(CMD_F);
  await pane.type("needle");
  assert.equal(pane.count(), "1/3", "three matches, the first one current");
  const highlights = pane.container.querySelectorAll(".xterm-decoration");
  assert.ok(highlights.length >= 3, `each match is highlighted in the pane: ${highlights.length}`);
  const caseToggle = pane.container.querySelector('[aria-label="Case sensitive"]') as HTMLButtonElement;
  caseToggle.click();
  await paint();
  assert.equal(caseToggle.getAttribute("aria-pressed"), "true");
  assert.equal(pane.count(), "1/2", "Needle no longer matches needle");
  await pane.type("nothing-like-this");
  assert.equal(pane.count(), "No results");
});

test("Enter walks to the next match and Shift+Enter back, wrapping; the arrows do the same", async () => {
  const pane = await mountPane(true, OUTPUT);
  await pane.key(CMD_F);
  await pane.type("needle");
  assert.equal(pane.count(), "1/3");
  await pane.key({ code: "Enter", key: "Enter" });
  assert.equal(pane.count(), "2/3");
  await pane.key({ code: "Enter", key: "Enter" });
  assert.equal(pane.count(), "3/3");
  await pane.key({ code: "Enter", key: "Enter" });
  assert.equal(pane.count(), "1/3", "past the last match it wraps to the first");
  await pane.key({ code: "Enter", key: "Enter", shiftKey: true });
  assert.equal(pane.count(), "3/3", "and back from the first to the last");
  (pane.container.querySelector('[aria-label="Previous match"]') as HTMLButtonElement).click();
  await paint();
  assert.equal(pane.count(), "2/3");
  (pane.container.querySelector('[aria-label="Next match"]') as HTMLButtonElement).click();
  await paint();
  assert.equal(pane.count(), "3/3");
});

test("Esc closes the bar and hands the keyboard back to the pane; reopening keeps the query", async () => {
  const pane = await mountPane(true, OUTPUT);
  await pane.key(CMD_F);
  await pane.type("needle");
  await pane.key({ code: "Escape", key: "Escape" });
  assert.equal(pane.bar(), null, "Esc closed the bar");
  assert.equal(focused(), "xterm-helper-textarea", "the pane has the keyboard again");
  press(pane.terminal, CMD_F);
  await paint();
  assert.equal(pane.query()?.value, "needle", "the query survived the close");
  assert.equal(pane.count(), "1/3");
});

test("the close button closes the bar and returns the keyboard too", async () => {
  const pane = await mountPane(true, OUTPUT);
  await pane.key(CMD_F);
  (pane.container.querySelector('[aria-label="Close search"]') as HTMLButtonElement).click();
  await settle();
  assert.equal(pane.bar(), null);
  assert.equal(focused(), "xterm-helper-textarea");
});

test("the find chord on an open bar reselects the query instead of closing it", async () => {
  const pane = await mountPane(true, OUTPUT);
  await pane.key(CMD_F);
  await pane.type("needle");
  pane.terminal.focus();
  press(pane.terminal, CMD_F);
  await settle();
  assert.ok(pane.bar(), "still open");
  assert.equal(focused(), "terminal-search-query", "the query took the keyboard back from the pane");
  const field = pane.query()!;
  field.setSelectionRange(0, 0);
  press(field, CMD_F);
  assert.deepEqual([field.selectionStart, field.selectionEnd], [0, "needle".length], "and its text is selected");
});
