// A pane strip's "+" menu in the whole window (`main.tsx`), mounted off
// macOS over the fake host: it stays open across a tab-strip answer that
// changes nothing, and a pick in a pane that is not focused acts in that
// pane and hands it the keyboard.
//
// The fake host answers `shell_open` as the real one does: a new shell tab,
// focused, in the folder the target names.

import "./window.ts";
import { drain, host, mountWindow, press, showTab, until } from "./window_host.ts";

import assert from "node:assert/strict";
import { test } from "node:test";

type Callback = (payload: unknown) => void;
type Target = { kind: "session"; id: string } | { kind: "shell"; key: string };

const internals = (window as unknown as { __TAURI_INTERNALS__: { invoke: (c: string, a?: Record<string, unknown>) => Promise<unknown> } })
  .__TAURI_INTERNALS__;
const hostInvoke = internals.invoke;
const shellOpens: Target[] = [];
const FOLDERS: Record<string, string> = { "u-1": "/a/one", "u-2": "/a/two", "u-3": "/b/one" };

/** The host's strip, as fresh objects, the way a real answer crosses IPC. */
function emitTabs(focus: string | null): void {
  for (const handler of host.events.get("terminal_tabs") ?? []) {
    (handler as Callback)({ event: "terminal_tabs", id: 0, payload: { tabs: JSON.parse(JSON.stringify(host.tabs)), focus } });
  }
}

internals.invoke = async (command, args = {}) => {
  if (command !== "shell_open") return hostInvoke(command, args);
  const target = args.target as Target;
  shellOpens.push(target);
  const key = `ainb-dsh-${shellOpens.length}`;
  const dir = target.kind === "session" ? FOLDERS[target.id] : "/elsewhere";
  host.tabs = [...host.tabs, { key, target: { kind: "shell", tmux: key, dir }, state: "attached" } as never];
  setTimeout(() => emitTabs(key), 0);
  return key;
};

const menu = () => document.querySelector<HTMLElement>(".tab-create-menu");
const plusIn = (group: string) => document.querySelector<HTMLButtonElement>(`.pane-group[data-group="${group}"] button.tab-new`)!;
const focusedGroup = () => document.querySelector<HTMLElement>(".pane-group[data-focused]")?.dataset.group;
const groupTabs = (group: string) =>
  [...document.querySelectorAll<HTMLElement>(`.pane-group[data-group="${group}"] .tab[data-key]`)].map((tab) => tab.dataset.key);

test("an open + menu stays open across a tab-strip answer that changes nothing", async () => {
  await mountWindow();
  await showTab("u-1");
  plusIn("g1").click();
  await until(() => menu() !== null, "the menu");
  // The same strip again, as new objects: nothing a person would see moved.
  emitTabs(null);
  await drain();
  assert.ok(menu() !== null, "the menu is still open");
  // A sessions frame that changes nothing, too.
  host.frames?.onmessage({
    frames: [{ section: "sessions", version: ++host.version, epoch: 1, host_id: "host-1", daemon_read: null, body: sessionsAgain() }],
  });
  await drain();
  assert.ok(menu() !== null, "the menu is still open after a sessions frame");
  menu()!.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  await until(() => menu() === null, "Esc to close the menu");
});

test("Tab out of the menu leaves the keyboard on its +, not on the page", async () => {
  plusIn("g1").click();
  await until(() => menu() !== null && document.activeElement?.closest(".tab-create-menu") !== null, "the menu, focused");
  const tab = new window.KeyboardEvent("keydown", { key: "Tab", bubbles: true, cancelable: true });
  document.activeElement!.dispatchEvent(tab);
  await until(() => menu() === null, "Tab to close the menu");
  // `ok`, not `equal`: a failed `equal` diffs two whole happy-dom nodes,
  // which takes longer than any test timeout.
  assert.ok(document.activeElement === plusIn("g1"), `the + has the keyboard, not ${document.activeElement?.tagName}`);
});

test("a pick in a pane that is not focused acts in that pane and gives it the keyboard", async () => {
  await showTab("u-2");
  // Split u-2 out: g1 holds u-1, g2 u-2, focused. Then focus g1.
  press({ code: "KeyD", key: "D", ctrlKey: true, shiftKey: true });
  await until(() => document.querySelectorAll(".pane-group").length === 2, "the split");
  document.querySelector<HTMLElement>('.pane-group[data-group="g1"] .tab[data-key="tmux_u-1"] .tab-title')!.click();
  await until(() => focusedGroup() === "g1", "g1 focused");
  await drain();
  plusIn("g2").click();
  await until(() => menu() !== null, "g2's menu");
  menu()!.querySelector<HTMLElement>('[data-item="terminal"]')!.click();
  await until(() => shellOpens.length > 0 && groupTabs("g2").includes("ainb-dsh-1"), `the shell in g2, not ${groupTabs("g1")}`);
  assert.deepEqual(shellOpens.at(-1), { kind: "session", id: "u-2" }, "in g2's worktree");
  assert.equal(focusedGroup(), "g2");
  const keyboardIn = () => document.activeElement?.closest<HTMLElement>(".terminal[data-tab]")?.dataset.tab ?? "";
  await until(() => ["tmux_u-2", "ainb-dsh-1"].includes(keyboardIn()), `g2's terminal to have the keyboard, not ${keyboardIn() || "none"}`);
});

function sessionsAgain() {
  const row = (id: string, path: string) => ({
    id,
    name: id,
    status: "Running",
    branch_name: `ainb/${id}`,
    workspace_path: path,
    tmux_session_name: `tmux_${id}`,
    attention: [],
  });
  return {
    workspaces: [
      { name: "a", path: "/a", shell_session: null, sessions: [row("u-1", "/a/one"), row("u-2", "/a/two")] },
      { name: "b", path: "/b", shell_session: null, sessions: [row("u-3", "/b/one")] },
    ],
    selected_session_id: host.selected,
    shell_selected: false,
  };
}
