// Terminal zoom, driven as a person drives it: the zoom chord pressed in a
// mounted, focused pane (`TerminalView`, as the window mounts it). The size is
// read back from the stylesheet xterm draws the pane with, so it is the font
// the pane shows, not a number the component kept for itself.

import { hostCalls, settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent, createSignal } from "solid-js";
import { render } from "solid-js/web";
import { FitAddon } from "@xterm/addon-fit";
import type { Accelerator } from "../../src/tabs.ts";
import { TerminalView } from "../../src/terminal.tsx";
import type { ByteChannel } from "../../src/transport.ts";

// The pane watches its host's size; happy-dom has the observer, Node does not.
Object.defineProperty(globalThis, "ResizeObserver", {
  value: (window as unknown as { ResizeObserver: unknown }).ResizeObserver,
  configurable: true,
  writable: true,
});

const cleanups: (() => void)[] = [];
afterEach(() => {
  for (const cleanup of cleanups.splice(0)) cleanup();
  hostCalls.clear();
  document.body.innerHTML = "";
});

/** Let xterm repaint: its renderer runs on timers. */
const paint = () => new Promise((done) => setTimeout(done, 50));

type Chord = { code: string; key: string; keyCode?: number; metaKey?: boolean; ctrlKey?: boolean; shiftKey?: boolean };

const CMD_EQUAL: Chord = { code: "Equal", key: "=", metaKey: true };
const CMD_PLUS: Chord = { code: "Equal", key: "+", metaKey: true, shiftKey: true };
const CMD_MINUS: Chord = { code: "Minus", key: "-", metaKey: true };
const CMD_0: Chord = { code: "Digit0", key: "0", metaKey: true };
const CTRL_EQUAL: Chord = { code: "Equal", key: "=", ctrlKey: true };
const CTRL_PLUS: Chord = { code: "Equal", key: "+", ctrlKey: true, shiftKey: true };
const CTRL_MINUS: Chord = { code: "Minus", key: "-", ctrlKey: true };
const CTRL_0: Chord = { code: "Digit0", key: "0", ctrlKey: true };
/** Ctrl+_, readline's and emacs's undo: xterm sends it as 0x1f. xterm drops
 * a keydown with no legacy key code, so this one carries the browser's. */
const CTRL_UNDERSCORE: Chord = { code: "Minus", key: "_", keyCode: 189, ctrlKey: true, shiftKey: true };

function press(target: Element, chord: Chord): KeyboardEvent {
  const event = new window.KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...chord });
  target.dispatchEvent(event);
  return event as unknown as KeyboardEvent;
}

/** One attached pane on `mac` or not, focused, with `onAccelerator` recorded. */
async function mountPane(mac: boolean, key = "tmux_app", active: () => boolean = () => true) {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const accelerators: Accelerator[] = [];
  cleanups.push(
    render(
      () =>
        createComponent(TerminalView, {
          tab: { key, target: { kind: "tmux", tmux: key }, state: "attached" } as never,
          title: "app",
          get active() {
            return active();
          },
          mac,
          onAccelerator: (shell: Accelerator) => accelerators.push(shell),
          onLeave() {},
          focusRef() {},
          theme: "dark",
        }),
      container,
    ),
  );
  await settle();
  const output = (hostCalls.get("terminal_output") as { bytes: ByteChannel } | undefined)?.bytes;
  assert.ok(output, "the pane opened its output channel");
  output.onmessage(new TextEncoder().encode("hello\r\n").buffer as ArrayBuffer);
  await paint();
  const terminal = container.querySelector(".xterm-helper-textarea") as HTMLTextAreaElement;
  assert.ok(terminal, "xterm mounted its keyboard target");
  terminal.focus();
  const owner = [...(container.querySelector(".xterm")?.classList ?? [])].find((name) =>
    name.startsWith("xterm-dom-renderer-owner-"),
  );
  assert.ok(owner, "xterm drew the pane with its DOM renderer");
  /** The font size xterm's stylesheet for this pane draws with, in px. */
  const fontSize = () => {
    for (const style of document.querySelectorAll("style")) {
      const text = style.textContent ?? "";
      if (!text.includes(owner)) continue;
      const size = /font-size:\s*(\d+)px/.exec(text);
      if (size) return Number(size[1]);
    }
    assert.fail(`no stylesheet sizes ${owner}`);
  };
  const zoom = async (chord: Chord) => {
    const event = press(terminal, chord);
    await paint();
    return event;
  };
  return { container, terminal, fontSize, zoom, accelerators };
}

test("Cmd+= and Cmd++ grow the focused pane's font a point each, Cmd+- shrinks it, Cmd+0 resets", async () => {
  const pane = await mountPane(true);
  assert.equal(pane.fontSize(), 13, "a pane opens at the base size");
  const event = await pane.zoom(CMD_EQUAL);
  assert.equal(pane.fontSize(), 14);
  assert.equal(event.defaultPrevented, true, "the pane does not also take the key");
  await pane.zoom(CMD_PLUS);
  assert.equal(pane.fontSize(), 15);
  await pane.zoom(CMD_MINUS);
  assert.equal(pane.fontSize(), 14);
  await pane.zoom(CMD_EQUAL);
  await pane.zoom(CMD_EQUAL);
  await pane.zoom(CMD_0);
  assert.equal(pane.fontSize(), 13, "reset goes back to the base size");
});

test("off macOS Ctrl+=, Ctrl++ and Ctrl+- zoom and Ctrl+0 resets, as Orca binds them", async () => {
  const pane = await mountPane(false);
  await pane.zoom(CTRL_EQUAL);
  await pane.zoom(CTRL_PLUS);
  assert.equal(pane.fontSize(), 15);
  const event = await pane.zoom(CTRL_MINUS);
  assert.equal(pane.fontSize(), 14);
  assert.equal(event.defaultPrevented, true);
  await pane.zoom(CTRL_0);
  assert.equal(pane.fontSize(), 13);
});

test("off macOS Ctrl+Shift+- is Ctrl+_: it reaches the shell as its undo byte and does not zoom", async () => {
  const pane = await mountPane(false);
  hostCalls.delete("terminal_input");
  await pane.zoom(CTRL_UNDERSCORE);
  assert.equal(pane.fontSize(), 13, "no zoom");
  assert.equal((hostCalls.get("terminal_input") as { data: string } | undefined)?.data, "\x1f");
});

test("zoom stops at Orca's bounds, 8 and 32 points", async () => {
  const pane = await mountPane(true);
  for (let press = 0; press < 30; press += 1) await pane.zoom(CMD_EQUAL);
  assert.equal(pane.fontSize(), 32);
  for (let press = 0; press < 30; press += 1) await pane.zoom(CMD_MINUS);
  assert.equal(pane.fontSize(), 8);
});

test("a zoom refits the grid and tells the shell its new size", async () => {
  const pane = await mountPane(true);
  // happy-dom lays nothing out, so every element reads as hidden; this one is shown.
  Object.defineProperty(pane.container.querySelector(".xterm-host"), "offsetParent", { get: () => document.body });
  const fit = FitAddon.prototype.fit;
  let fits = 0;
  FitAddon.prototype.fit = function (this: FitAddon) {
    fits += 1;
    return fit.call(this);
  };
  try {
    hostCalls.delete("terminal_resize");
    await pane.zoom(CMD_EQUAL);
    assert.equal(fits, 1, "the pane refit to the new cell size");
    assert.ok(hostCalls.get("terminal_resize"), "and sent the resulting grid to the shell");
    await pane.zoom(CMD_0);
    assert.equal(fits, 2, "a reset refits too");
  } finally {
    FitAddon.prototype.fit = fit;
  }
});

test("the size lives with its pane: another pane keeps its own, and a hidden pane keeps its zoom", async () => {
  const [shown, setShown] = createSignal(true);
  const first = await mountPane(true, "tmux_one", shown);
  const second = await mountPane(true, "tmux_two");
  first.terminal.focus();
  await first.zoom(CMD_EQUAL);
  await first.zoom(CMD_EQUAL);
  assert.equal(first.fontSize(), 15);
  assert.equal(second.fontSize(), 13, "zoom acts on the focused pane only");
  setShown(false);
  await settle();
  setShown(true);
  await settle();
  assert.equal(first.fontSize(), 15, "a tab switch away and back keeps the zoom");
});

test("a new pane opens at the base size: zoom is never saved, as in Orca", async () => {
  const pane = await mountPane(true);
  await pane.zoom(CMD_EQUAL);
  for (const cleanup of cleanups.splice(0)) cleanup();
  document.body.innerHTML = "";
  const again = await mountPane(true);
  assert.equal(again.fontSize(), 13);
});

test("the shell's chords and the find chord still do their own thing, and no zoom", async () => {
  const pane = await mountPane(true);
  await pane.zoom({ code: "KeyU", key: "u", metaKey: true });
  assert.deepEqual(pane.accelerators, [{ kind: "attention" }], "Cmd+U still jumps to attention");
  await pane.zoom({ code: "KeyF", key: "f", metaKey: true });
  assert.ok(pane.container.querySelector(".terminal-search"), "Cmd+F still opens find");
  assert.equal(pane.fontSize(), 13, "neither zoomed");
  pane.terminal.focus();
  await pane.zoom(CMD_EQUAL);
  assert.equal(pane.accelerators.length, 1, "a zoom chord is no shell accelerator");
  assert.equal(pane.fontSize(), 14);
});

test("off macOS Ctrl+Shift+U and Ctrl+Shift+F keep their chords too", async () => {
  const pane = await mountPane(false);
  await pane.zoom({ code: "KeyU", key: "U", ctrlKey: true, shiftKey: true });
  assert.deepEqual(pane.accelerators, [{ kind: "attention" }]);
  await pane.zoom({ code: "KeyF", key: "F", ctrlKey: true, shiftKey: true });
  assert.ok(pane.container.querySelector(".terminal-search"));
  assert.equal(pane.fontSize(), 13);
});
