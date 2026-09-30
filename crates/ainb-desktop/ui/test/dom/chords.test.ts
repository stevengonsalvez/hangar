// The Orca-parity chords off macOS, pressed on the whole window over a fake
// host: each is Ctrl+Shift, this window's convention, and does what Orca's
// own chord does (`orca:src/shared/keybindings/`).

import "./window.ts";
import {
  drain,
  focused,
  host,
  mountWindow,
  paletteOpen,
  press,
  refusals,
  showTab,
  shownTerminal,
  sidebarShown,
  until,
} from "./window_host.ts";

import assert from "node:assert/strict";
import { before, test } from "node:test";

before(mountWindow);

const ctrlShift = (code: string, key: string) => ({ code, key, ctrlKey: true, shiftKey: true });

test("Ctrl+Shift+J opens the palette, Orca's worktree switcher, and closes it again", async () => {
  assert.equal(paletteOpen(), false);
  const event = press(ctrlShift("KeyJ", "J"));
  await until(paletteOpen, "the palette");
  assert.equal(event.defaultPrevented, true, "the chord was taken");
  press(ctrlShift("KeyJ", "J"));
  await until(() => !paletteOpen(), "the palette closed");
});

test("Ctrl+Shift+K no longer opens the palette, and outside a terminal is not taken", async () => {
  (document.activeElement as HTMLElement | null)?.blur();
  const event = press(ctrlShift("KeyK", "K"));
  await drain();
  assert.equal(paletteOpen(), false);
  assert.equal(event.defaultPrevented, false, "nothing to clear on the board: the key is left alone");
});

test("Ctrl+Shift+B hides the sidebar and shows it again", async () => {
  assert.equal(sidebarShown(), true);
  const event = press(ctrlShift("KeyB", "B"));
  await until(() => !sidebarShown(), "the sidebar hidden");
  assert.equal(event.defaultPrevented, true, "the chord was taken");
  press(ctrlShift("KeyB", "B"));
  await until(sidebarShown, "the sidebar shown again");
});

test("hiding the sidebar while it has the keyboard hands it to the shown terminal", async () => {
  await showTab("u-1");
  document.querySelector<HTMLElement>(".sidebar")!.focus();
  assert.equal(focused(), "sidebar");
  press(ctrlShift("KeyB", "B"));
  await until(() => !sidebarShown(), "the sidebar hidden");
  await until(() => focused() === "xterm-helper-textarea", "u-1's terminal focused");
  press(ctrlShift("KeyB", "B"));
  await until(sidebarShown, "the sidebar shown again");
});

test("Esc Esc from a terminal with the sidebar hidden shows it and gives it the keyboard", async () => {
  await showTab("u-1");
  press(ctrlShift("KeyB", "B"));
  await until(() => !sidebarShown(), "the sidebar hidden");
  await until(() => focused() === "xterm-helper-textarea", "u-1's terminal focused");
  press({ code: "Escape", key: "Escape" });
  press({ code: "Escape", key: "Escape" });
  await until(() => sidebarShown() && focused() === "sidebar", "the sidebar shown and focused");
});

test("off macOS Ctrl+Shift+Up and Ctrl+Shift+Down stay the pane's: no worktree moves", async () => {
  await showTab("u-1");
  host.sent = [];
  host.typed = [];
  for (const code of ["ArrowUp", "ArrowDown"]) press({ code, key: code, ctrlKey: true, shiftKey: true });
  await drain();
  assert.deepEqual(host.typed, ["\x1b[1;6A", "\x1b[1;6B"], "the pane got both keys");
  assert.deepEqual(host.sent, [], "nothing sent");
  assert.equal(shownTerminal(), "tmux_u-1");
  assert.deepEqual(refusals(), []);
});
