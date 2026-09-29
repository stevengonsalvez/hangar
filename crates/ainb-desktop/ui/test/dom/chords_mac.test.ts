// The Orca-parity chords on macOS, pressed on the whole window over a fake
// host mounted as macOS (Cmd, not Ctrl+Shift). Each does what Orca's own
// chord does (`orca:src/shared/keybindings/`).

import "./window.ts";
import {
  drain,
  host,
  mountWindow,
  paletteOpen,
  press,
  pretendMac,
  refusals,
  settingsShown,
  show,
  showTab,
  shownTerminal,
  sidebarShown,
  tabOf,
  until,
} from "./window_host.ts";

import assert from "node:assert/strict";
import { before, test } from "node:test";

before(async () => {
  pretendMac();
  await mountWindow();
});

/** Close u-3's tab, as the host does when its session ends: back to u-1 and u-2. */
async function closeU3(): Promise<void> {
  host.tabs = host.tabs.filter((tab) => tab.target.id !== "u-3");
  for (const handler of host.events.get("terminal_tabs") ?? []) {
    handler({ event: "terminal_tabs", id: 0, payload: { tabs: host.tabs, focus: null } });
  }
  await until(() => document.querySelectorAll(".tab[data-state]").length === host.tabs.length, "u-3's tab closed");
  await drain();
}

const cmd = (code: string, key: string) => ({ code, key, metaKey: true });
const cmdShift = (code: string, key: string) => ({ code, key, metaKey: true, shiftKey: true });

test("Cmd+J opens the palette, Orca's worktree switcher, and closes it again", async () => {
  press(cmd("KeyJ", "j"));
  await until(paletteOpen, "the palette");
  press(cmd("KeyJ", "j"));
  await until(() => !paletteOpen(), "the palette closed");
});

test("Cmd+K no longer opens the palette", async () => {
  press(cmd("KeyK", "k"));
  await drain();
  assert.equal(paletteOpen(), false);
});

test("Cmd+B hides the sidebar and shows it again", async () => {
  press(cmd("KeyB", "b"));
  await until(() => !sidebarShown(), "the sidebar hidden");
  press(cmd("KeyB", "b"));
  await until(sidebarShown, "the sidebar shown again");
});

test("Cmd+Shift+Down and Cmd+Shift+Up walk the sidebar's worktrees, opening one with no tab", async () => {
  await showTab("u-1");
  press(cmdShift("ArrowDown", "ArrowDown"));
  await until(() => shownTerminal() === tabOf("u-2").key, "the next worktree's tab");
  await drain();
  host.sent = [];
  press(cmdShift("ArrowDown", "ArrowDown"));
  // u-3, in the next project, has no tab: opened through its row, after home.
  await until(() => shownTerminal() === tabOf("u-3").key, "u-3 opened and shown");
  assert.deepEqual(host.sent.slice(0, 2), [{ id: "answer_home" }, { id: "session_list.select_row", session: "u-3", open: true }]);
  press(cmdShift("ArrowDown", "ArrowDown"));
  await until(() => shownTerminal() === tabOf("u-1").key, "wrapped to the first worktree");
  press(cmdShift("ArrowUp", "ArrowUp"));
  await until(() => shownTerminal() === tabOf("u-3").key, "up wraps to the last worktree");
  await drain();
  assert.deepEqual(refusals(), []);
  await closeU3();
});

test("two quick Cmd+Shift+Down presses open a tabless worktree once, and step on past it", async () => {
  await showTab("u-2");
  host.sent = [];
  press(cmdShift("ArrowDown", "ArrowDown"));
  press(cmdShift("ArrowDown", "ArrowDown"));
  await until(() => host.tabs.length === 3, "u-3's tab opened");
  await drain();
  const opens = host.sent.filter((sent) => sent.id === "session_list.select_row" && sent.open);
  assert.deepEqual(opens, [{ id: "session_list.select_row", session: "u-3", open: true }], "u-3 opened once");
  assert.ok(
    host.sent.some((sent) => sent.id === "session_list.select_row" && sent.session === "u-1"),
    "the second press went on to u-1",
  );
  await closeU3();
});

test("in a text field Cmd+Shift+Up and Down select, as macOS does: no worktree moves", async () => {
  await showTab("u-1");
  show("config");
  await until(settingsShown, "the settings page");
  const search = document.querySelector<HTMLInputElement>(".settings-search")!;
  search.focus();
  host.sent = [];
  for (const code of ["ArrowUp", "ArrowDown"]) {
    const event = press(cmdShift(code, code));
    assert.equal(event.defaultPrevented, false, `${code} was taken from the field`);
  }
  await drain();
  assert.deepEqual(host.sent, [], "nothing sent");
  assert.ok(settingsShown(), "still on Settings");
});

test("Cmd+Shift+Down from Settings leaves the page first, and nothing is refused", async () => {
  await showTab("u-1");
  show("config");
  await until(settingsShown, "the settings page");
  host.sent = [];
  press(cmdShift("ArrowDown", "ArrowDown"));
  await until(() => !settingsShown() && shownTerminal() === tabOf("u-2").key, "Settings closed and u-2 shown");
  await drain();
  assert.deepEqual(host.sent[0], { id: "answer_home" });
  assert.deepEqual(refusals(), []);
});
