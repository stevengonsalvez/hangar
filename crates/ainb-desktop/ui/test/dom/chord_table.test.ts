// The whole chord table, every key under every modifier set on both
// platforms: no two actions answer one chord, and no chord takes a key the
// terminal would have sent its pane. The table is what the window runs, the
// shell's `accelerator` and the find chord, not a list kept beside them.

import "./window.ts";

import assert from "node:assert/strict";
import { test } from "node:test";
import { Terminal } from "@xterm/xterm";
import { accelerator } from "../../src/tabs.ts";
import { findChord } from "../../src/terminal_search.tsx";

Object.defineProperty(globalThis, "ResizeObserver", {
  value: (window as unknown as { ResizeObserver: unknown }).ResizeObserver,
  configurable: true,
  writable: true,
});

/** Each key: its `KeyboardEvent.keyCode`, which xterm reads, and its `key`
 * plain and shifted on a US layout. */
const KEYS: Record<string, [keyCode: number, key: string, shifted: string]> = {
  ...Object.fromEntries(
    "ABCDEFGHIJKLMNOPQRSTUVWXYZ".split("").map((c) => [`Key${c}`, [c.charCodeAt(0), c.toLowerCase(), c]]),
  ),
  ...Object.fromEntries(")!@#$%^&*(".split("").map((shifted, n) => [`Digit${n}`, [48 + n, String(n), shifted]])),
  BracketLeft: [219, "[", "{"],
  BracketRight: [221, "]", "}"],
  Minus: [189, "-", "_"],
  Equal: [187, "=", "+"],
  Comma: [188, ",", "<"],
  Period: [190, ".", ">"],
  Slash: [191, "/", "?"],
  Backslash: [220, "\\", "|"],
  Semicolon: [186, ";", ":"],
  Quote: [222, "'", '"'],
  Backquote: [192, "`", "~"],
  Space: [32, " ", " "],
  Tab: [9, "Tab", "Tab"],
  Enter: [13, "Enter", "Enter"],
  Escape: [27, "Escape", "Escape"],
  Backspace: [8, "Backspace", "Backspace"],
  ArrowUp: [38, "ArrowUp", "ArrowUp"],
  ArrowDown: [40, "ArrowDown", "ArrowDown"],
  ArrowLeft: [37, "ArrowLeft", "ArrowLeft"],
  ArrowRight: [39, "ArrowRight", "ArrowRight"],
  PageUp: [33, "PageUp", "PageUp"],
  PageDown: [34, "PageDown", "PageDown"],
  Home: [36, "Home", "Home"],
  End: [35, "End", "End"],
};

type Mods = { metaKey: boolean; ctrlKey: boolean; shiftKey: boolean; altKey: boolean };

/** Every modifier set: none of them up to all four. */
const MODS: Mods[] = Array.from({ length: 16 }, (_, bits) => ({
  metaKey: (bits & 1) !== 0,
  ctrlKey: (bits & 2) !== 0,
  shiftKey: (bits & 4) !== 0,
  altKey: (bits & 8) !== 0,
}));

const spell = (code: string, mods: Mods) =>
  [mods.metaKey && "Cmd", mods.ctrlKey && "Ctrl", mods.altKey && "Alt", mods.shiftKey && "Shift", code]
    .filter(Boolean)
    .join("+");

/** Every chord the window answers on `mac`, and what answers it. */
function claimed(mac: boolean): { code: string; mods: Mods; actions: string[] }[] {
  const out = [];
  for (const code of Object.keys(KEYS)) {
    for (const mods of MODS) {
      const event = { code, ...mods };
      const shell = accelerator(event, mac);
      const actions = [...(shell === null ? [] : [`shell:${shell.kind}`]), ...(findChord(event, mac) ? ["find"] : [])];
      if (actions.length > 0) out.push({ code, mods, actions });
    }
  }
  return out;
}

for (const mac of [true, false]) {
  const platform = mac ? "macOS" : "off macOS";

  test(`${platform}: no two actions answer the same chord`, () => {
    const clashes = claimed(mac)
      .filter((chord) => chord.actions.length > 1)
      .map((chord) => `${spell(chord.code, chord.mods)}: ${chord.actions.join(", ")}`);
    assert.deepEqual(clashes, []);
  });

  test(`${platform}: every action the table names is reachable`, () => {
    const actions = new Set(claimed(mac).flatMap((chord) => chord.actions));
    const expected = [
      "find",
      "shell:tab",
      "shell:prev",
      "shell:next",
      "shell:close",
      "shell:palette",
      "shell:new",
      "shell:attention",
      "shell:hosts",
      "shell:clear",
      "shell:sidebar",
      ...(mac ? ["shell:worktree"] : ["shell:copy", "shell:paste"]),
    ];
    assert.deepEqual([...actions].sort(), expected.sort());
  });
}

/**
 * Chords that do take a byte from the pane, each named with the byte, so a
 * new one fails here and this one is not forgotten. Ctrl+Shift+2 is Ctrl+@,
 * which xterm sends as NUL (emacs set-mark); the tab-2 chord has held it
 * since before the Orca chords, and Ctrl+Space sends the same NUL.
 */
const KNOWN_STOLEN: Record<string, string> = { "Ctrl+Shift+Digit2": "\x00" };

test("no chord in the table takes a key the terminal would send its pane", async () => {
  const host = document.createElement("div");
  document.body.append(host);
  const term = new Terminal();
  term.open(host);
  let sent = "";
  term.onData((data) => (sent += data));
  const target = host.querySelector(".xterm-helper-textarea") as HTMLElement;
  target.focus();

  const stolen: Record<string, string> = {};
  // Both platforms' chords, each pressed in a bare xterm, with no shell
  // handler in front: what it sends is what the chord keeps from the pane.
  // xterm runs in its non-macOS mode here, which is the mode that sends bytes
  // for Ctrl chords; a Cmd chord sends none in either.
  for (const { code, mods } of [...claimed(false), ...claimed(true)]) {
    const [keyCode, plain, shifted] = KEYS[code];
    const event = new window.KeyboardEvent("keydown", {
      code,
      key: mods.shiftKey ? shifted : plain,
      bubbles: true,
      cancelable: true,
      ...mods,
    });
    // happy-dom leaves `keyCode` 0; xterm decides by it.
    Object.defineProperty(event, "keyCode", { value: keyCode });
    sent = "";
    target.dispatchEvent(event);
    if (sent !== "") stolen[spell(code, mods)] = sent;
  }
  term.dispose();
  host.remove();
  assert.deepEqual(stolen, KNOWN_STOLEN);
});

test("the xterm probe is live: the pane's own keys do send bytes", () => {
  // Guards the test above against a probe that sends nothing for anything.
  const host = document.createElement("div");
  document.body.append(host);
  const term = new Terminal();
  term.open(host);
  let sent = "";
  term.onData((data) => (sent += data));
  const target = host.querySelector(".xterm-helper-textarea") as HTMLElement;
  target.focus();
  const probe = (code: string, key: string, keyCode: number, mods: Partial<Mods>) => {
    const event = new window.KeyboardEvent("keydown", { code, key, bubbles: true, cancelable: true, ...mods });
    Object.defineProperty(event, "keyCode", { value: keyCode });
    sent = "";
    target.dispatchEvent(event);
    return sent;
  };
  assert.equal(probe("KeyC", "c", 67, { ctrlKey: true }), "\x03", "Ctrl+C");
  assert.equal(probe("Minus", "_", 189, { ctrlKey: true, shiftKey: true }), "\x1f", "Ctrl+_, the undo #254 kept");
  assert.equal(probe("ArrowUp", "ArrowUp", 38, { ctrlKey: true, shiftKey: true }), "\x1b[1;6A", "Ctrl+Shift+Up, left to the pane");
  term.dispose();
  host.remove();
});
