// The recent-tab switcher's order and steps, and the closed-tab stack: what
// comes back, how, and where.

import assert from "node:assert/strict";
import { test } from "node:test";
import type { SessionsView_Serialize } from "../../../ainb-app/bindings/AppState";
import { groups, initialLayout, splitGroup } from "./layout.ts";
import {
  closedFrom,
  MAX_CLOSED,
  placeReopened,
  popReopenable,
  pushClosed,
  releasesSwitcher,
  reopenOf,
  stepIndex,
  switcherOrder,
  type ClosedTab,
} from "./recent_tabs.ts";
import { accelerator, type Tab } from "./tabs.ts";

const session = (id: string): Tab => ({ key: `tmux_${id}`, target: { kind: "session", id, tmux: `tmux_${id}` }, state: "attached" });
const shell = (key: string, dir: string): Tab => ({ key, target: { kind: "shell", tmux: key, dir }, state: "attached" });
const listing = (rows: [id: string, path: string][]) =>
  ({
    workspaces: [{ name: "a", path: "/a", shell_session: null, sessions: rows.map(([id, path]) => ({ id, workspace_path: path })) }],
    selected_session_id: null,
    shell_selected: false,
  }) as unknown as SessionsView_Serialize;
const closed = (tab: Tab, group: string | null = "g1", index = 0): ClosedTab => ({ key: tab.key, target: tab.target, group, index });

test("the switcher lists the shown tab first, then the pane's tabs most recently shown first", () => {
  // Shown in the order a, b, c, a, d: d is shown now.
  const recent = ["b", "c", "a", "d"];
  assert.deepEqual(switcherOrder(["a", "b", "c", "d", "e"], recent, "d"), ["d", "a", "c", "b", "e"]);
  // Only the pane's own tabs, however recently another pane's was shown.
  assert.deepEqual(switcherOrder(["a", "c"], recent, "c"), ["c", "a"]);
  // A shown tab the recent list has not caught up with still leads.
  assert.deepEqual(switcherOrder(["a", "b"], ["a"], "b"), ["b", "a"]);
  assert.deepEqual(switcherOrder(["a", "b"], [], null), ["a", "b"]);
});

test("steps wrap both ways, and from no entry start at either end", () => {
  assert.equal(stepIndex(3, 0, 1), 1);
  assert.equal(stepIndex(3, 2, 1), 0);
  assert.equal(stepIndex(3, 0, -1), 2);
  assert.equal(stepIndex(3, -1, 1), 0);
  assert.equal(stepIndex(3, -1, -1), 2);
  assert.equal(stepIndex(0, 0, 1), -1);
});

test("letting go of Ctrl commits; a Tab keyup counts once Ctrl is up", () => {
  assert.equal(releasesSwitcher({ type: "keyup", key: "Control", ctrlKey: false }), true);
  assert.equal(releasesSwitcher({ type: "keyup", key: "Tab", ctrlKey: false }), true);
  assert.equal(releasesSwitcher({ type: "keyup", key: "Tab", ctrlKey: true }), false);
  assert.equal(releasesSwitcher({ type: "keyup", key: "Shift", ctrlKey: true }), false);
  assert.equal(releasesSwitcher({ type: "keydown", key: "Control", ctrlKey: true }), false);
});

test("Ctrl+Tab and reopen are the shell's chords on both platforms", () => {
  const key = (code: string, mods: { meta?: boolean; ctrl?: boolean; shift?: boolean; alt?: boolean }) => ({
    code,
    metaKey: mods.meta ?? false,
    ctrlKey: mods.ctrl ?? false,
    shiftKey: mods.shift ?? false,
    altKey: mods.alt ?? false,
  });
  for (const mac of [true, false]) {
    assert.deepEqual(accelerator(key("Tab", { ctrl: true }), mac), { kind: "recent", step: 1 });
    assert.deepEqual(accelerator(key("Tab", { ctrl: true, shift: true }), mac), { kind: "recent", step: -1 });
    assert.equal(accelerator(key("Tab", {}), mac), null, "plain Tab is the pane's");
  }
  assert.deepEqual(accelerator(key("KeyT", { meta: true, shift: true }), true), { kind: "reopen" });
  assert.deepEqual(accelerator(key("KeyT", { ctrl: true, alt: true, shift: true }), false), { kind: "reopen" });
  // Mod+T stays New terminal on both.
  assert.deepEqual(accelerator(key("KeyT", { meta: true }), true), { kind: "terminal" });
  assert.deepEqual(accelerator(key("KeyT", { ctrl: true, shift: true }), false), { kind: "terminal" });
});

test("the closed stack keeps the newest ten", () => {
  let stack: ClosedTab[] = [];
  for (let n = 0; n < MAX_CLOSED + 3; n += 1) stack = pushClosed(stack, closed(session(`u-${n}`)));
  assert.equal(MAX_CLOSED, 10);
  assert.equal(stack.length, MAX_CLOSED);
  assert.equal(stack[0].key, "tmux_u-12", "newest first");
  assert.equal(stack.at(-1)!.key, "tmux_u-3", "the oldest three fell off");
});

test("a closed tab remembers its pane and its place there", () => {
  const layout = splitGroup(initialLayout(["a", "b", "c"]), "g1", "right");
  // The split took the shown a to g2, leaving b, c in g1.
  assert.deepEqual(
    groups(layout).map((group) => [group.id, group.tabs]),
    [
      ["g1", ["b", "c"]],
      ["g2", ["a"]],
    ],
  );
  const tab = { ...session("x"), key: "c" };
  assert.deepEqual(closedFrom(layout, tab), { key: "c", target: tab.target, group: "g1", index: 1 });
  assert.deepEqual(closedFrom(layout, { ...tab, key: "gone" }).group, null);
});

test("a session tab reattaches while its session is listed", () => {
  const sessions = listing([["u-1", "/a/one"]]);
  assert.deepEqual(reopenOf(closed(session("u-1")), [], sessions), { kind: "row", row: { session: "u-1" } });
  assert.equal(reopenOf(closed(session("u-9")), [], sessions), null, "a session gone from the list cannot come back");
  assert.equal(reopenOf(closed(session("u-1")), [session("u-1")], sessions), null, "a tab open again is not closed");
  const bare: Tab = { key: "t", target: { kind: "tmux", tmux: "t" }, state: "attached" };
  assert.deepEqual(reopenOf(closed(bare), [], sessions), { kind: "row", row: { other_tmux: "t" } });
});

test("a closed shell comes back as a fresh shell in its worktree, named by ids", () => {
  const sessions = listing([["u-1", "/a/one"]]);
  const gone = closed(shell("ainb-dsh-1", "/a/one"));
  assert.deepEqual(reopenOf(gone, [], sessions), { kind: "shell", target: { kind: "session", id: "u-1" } });
  // No session lists the folder: another shell open in it names it.
  const other = shell("ainb-dsh-2", "/b/two");
  assert.deepEqual(reopenOf(closed(shell("ainb-dsh-1", "/b/two")), [other], sessions), {
    kind: "shell",
    target: { kind: "shell", key: "ainb-dsh-2" },
  });
  assert.equal(reopenOf(closed(shell("ainb-dsh-1", "/c")), [other], sessions), null, "nothing names the folder");
});

test("reopen takes the newest entry that can come back, dropping those above it that cannot", () => {
  const sessions = listing([["u-1", "/a/one"]]);
  const stack = [closed(session("u-9")), closed(session("u-1")), closed(session("u-1"))];
  const { found, rest } = popReopenable(stack, [], sessions);
  assert.deepEqual(found?.reopen, { kind: "row", row: { session: "u-1" } });
  assert.deepEqual(rest, [stack[2]]);
  assert.deepEqual(popReopenable([closed(session("u-9"))], [], sessions), { found: null, rest: [] });
});

test("a reopened tab goes back to its pane and place once the layout holds it", () => {
  // g1: b, c, x | g2: a. `a` came back into the focused g2; it was g1's first.
  const layout = splitGroup(initialLayout(["a", "b", "c", "x"]), "g1", "right");
  const moved = placeReopened(layout, new Map([["a", closed(session("a"), "g1", 0)]]));
  assert.deepEqual(moved.placed, ["a"]);
  assert.deepEqual(
    groups(moved.layout).map((group) => [group.id, group.tabs]),
    [["g1", ["a", "b", "c", "x"]]],
    "back at g1's front; g2, emptied, closed",
  );
  const waiting = placeReopened(layout, new Map([["y", closed(session("y"))]]));
  assert.deepEqual(waiting, { layout, placed: [] }, "a key not back yet waits");
  const paneGone = placeReopened(layout, new Map([["c", closed(session("c"), "g9", 0)]]));
  assert.deepEqual(paneGone, { layout, placed: ["c"] }, "a pane closed since leaves the tab where it landed");
});
