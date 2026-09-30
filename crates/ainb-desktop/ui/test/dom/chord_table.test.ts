// The whole chord table, every key under every modifier set on both
// platforms: no two actions answer one chord, and no chord takes a key the
// terminal would have sent its pane. The table is what the window runs, the
// shell's `accelerator`, the find, zoom and menu chords, not a list kept
// beside them.

import "./window.ts";

import assert from "node:assert/strict";
import { test } from "node:test";
import { Terminal } from "@xterm/xterm";
import { accelerator } from "../../src/tabs.ts";
import { findChord } from "../../src/terminal_search.tsx";
import { zoomChord } from "../../src/terminal_zoom.ts";
import { menuChord } from "../../src/terminal_menu.tsx";

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
  NumpadAdd: [107, "+", "+"],
  NumpadSubtract: [109, "-", "-"],
  F10: [121, "F10", "F10"],
  ContextMenu: [93, "ContextMenu", "ContextMenu"],
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
      const [, plain, shifted] = KEYS[code];
      const event = { code, key: mods.shiftKey ? shifted : plain, ...mods };
      const shell = accelerator(event, mac);
      const zoom = zoomChord(event, mac);
      const actions = [
        ...(shell === null ? [] : [`shell:${shell.kind}`]),
        ...(findChord(event, mac) ? ["find"] : []),
        ...(zoom === null ? [] : [`zoom:${zoom}`]),
        ...(menuChord(event) ? ["menu"] : []),
      ];
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
      "zoom:in",
      "zoom:out",
      "zoom:reset",
      "menu",
      "shell:split",
      "shell:tab",
      "shell:prev",
      "shell:next",
      "shell:close",
      "shell:palette",
      "shell:new",
      "shell:terminal",
      "shell:attention",
      "shell:hosts",
      "shell:clear",
      "shell:sidebar",
      "shell:recent",
      "shell:reopen",
      ...(mac ? ["shell:worktree"] : ["shell:copy", "shell:paste"]),
    ];
    assert.deepEqual([...actions].sort(), expected.sort());
  });
}

/**
 * Chords that do take a byte from the pane, each named with the byte, so a
 * new one fails here and none is forgotten.
 */
const KNOWN_STOLEN: Record<string, string> = {
  // Ctrl+@, which xterm sends as NUL (emacs set-mark); the tab-2 chord has
  // held it since before the Orca chords, and Ctrl+Space sends the same NUL.
  "Ctrl+Shift+Digit2": "\x00",
  // Follows Orca split-down (orca `definitions-core-4.ts:33-44`): ESC D is
  // readline's M-D, kill-word.
  "Alt+Shift+KeyD": "\x1bD",
  // The context menu's keyboard chord, the platform's own menu key (#258).
  "Shift+F10": "\x1b[21;2~",
  // The recent-tab switcher, which Orca takes from its terminal too
  // (`allowInTerminal`, orca `definitions-core-2.ts:199-207`). Plain Tab and
  // Shift+Tab still send the pane these same bytes (the probe below).
  "Ctrl+Tab": "\t",
  "Ctrl+Shift+Tab": "\x1b[Z",
  // Reopen closed tab off macOS: Orca's Mod+Shift+T is this window's Mod+T
  // there, so it adds Alt. Ctrl+Alt+T still sends the pane this same C-M-t.
  "Ctrl+Alt+Shift+KeyT": "\x1b\x14",
};

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
  assert.equal(probe("Tab", "Tab", 9, {}), "\t", "Tab, the byte Ctrl+Tab would send");
  assert.equal(probe("Tab", "Tab", 9, { shiftKey: true }), "\x1b[Z", "Shift+Tab, the byte Ctrl+Shift+Tab would send");
  assert.equal(probe("KeyT", "t", 84, { ctrlKey: true, altKey: true }), "\x1b\x14", "Ctrl+Alt+T, the byte Ctrl+Alt+Shift+T would send");
  term.dispose();
  host.remove();
});
