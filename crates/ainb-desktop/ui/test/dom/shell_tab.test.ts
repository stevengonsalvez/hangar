// "New terminal" in the whole window (`main.tsx`), mounted over the fake host:
// the strip's + and Mod+T ask the host for a shell in the shown tab's
// worktree, the host's tab lands in the strip, and closing a shell's tab,
// by its x or by Mod+W, ends the shell (`shell_close`) where any other tab
// only detaches (`terminal_close`). A refusal shows the host's sentence.
//
// The fake host answers `shell_open` as the real one does: a new shell tab,
// focused, in the folder the target names.

import "./window.ts";
import { drain, host, mountWindow, press, showTab, until } from "./window_host.ts";

import assert from "node:assert/strict";
import { test } from "node:test";

/** The host's sentence for an open that may have made its shell
 * (`shell_tab::MAY_STILL_OPEN` and the daemon's detail). */
const MAY_STILL_OPEN_TEXT =
  "The terminal may still open; check before opening another. (tmux did not answer within 10s; the shell may still appear as ainb-dsh-0123abcd)";

type Callback = (payload: unknown) => void;
type Call = { command: string; args: Record<string, unknown> };
type Target = { kind: "session"; id: string } | { kind: "shell"; key: string };

const internals = (window as unknown as { __TAURI_INTERNALS__: { invoke: (c: string, a?: Record<string, unknown>) => Promise<unknown> } })
  .__TAURI_INTERNALS__;
const hostInvoke = internals.invoke;
const calls: Call[] = [];
/** A reply that rejects the next call of a command, as a refused command does. */
const refuse = new Map<string, string>();
let opened = 0;

/** The folder each session's worktree is in, as the fake host lists them. */
const FOLDERS: Record<string, string> = { "u-1": "/a/one", "u-2": "/a/two", "u-3": "/b/one" };

function emitTabs(focus: string | null): void {
  for (const handler of host.events.get("terminal_tabs") ?? []) {
    (handler as Callback)({ event: "terminal_tabs", id: 0, payload: { tabs: host.tabs, focus } });
  }
}

function folderOf(target: Target): string {
  if (target.kind === "session") return FOLDERS[target.id];
  const tab = host.tabs.find((one) => one.key === target.key) as unknown as { target: { dir: string } };
  return tab.target.dir;
}

internals.invoke = async (command, args = {}) => {
  if (command !== "shell_open" && command !== "shell_close" && command !== "terminal_close") {
    return hostInvoke(command, args);
  }
  calls.push({ command, args });
  const refusal = refuse.get(command);
  if (refusal !== undefined) {
    refuse.delete(command);
    throw refusal;
  }
  if (command === "shell_open") {
    opened += 1;
    const key = `ainb-dsh-0000000${opened}`;
    const dir = folderOf(args.target as Target);
    host.tabs = [...host.tabs, { key, target: { kind: "shell", tmux: key, dir }, state: "attached" } as never];
    setTimeout(() => emitTabs(key), 0);
    return key;
  }
  host.tabs = host.tabs.filter((tab) => tab.key !== args.key);
  setTimeout(() => emitTabs(null), 0);
  return null;
};

const plus = () => document.querySelector<HTMLButtonElement>(".tab-new-terminal");
const lastCall = (command: string) => [...calls].reverse().find((call) => call.command === command);
const shellTabs = () =>
  [...document.querySelectorAll<HTMLElement>(".tab[data-state]")].filter((tab) =>
    tab.querySelector(".tab-title")?.textContent?.startsWith("Terminal"),
  );
const toasts = () => [...document.querySelectorAll(".toast")].map((toast) => toast.textContent ?? "");
/** Mod+T and Mod+W as this window reads them off macOS: Ctrl+Shift. */
const chord = (code: string, key: string) => press({ code, key, ctrlKey: true, shiftKey: true });

test("the + is off with nothing to open in, and opens a shell in the shown tab's worktree", async () => {
  await mountWindow();
  // The board, and no row selected: no worktree is in view.
  assert.ok(plus(), "the window mounts the New terminal +");
  assert.equal(plus()!.disabled, true);

  await showTab("u-1");
  assert.equal(plus()!.disabled, false);
  plus()!.click();
  await until(() => shellTabs().length === 1, "the shell's tab in the strip");
  // Only ids cross: the host resolves the folder.
  assert.deepEqual(lastCall("shell_open")?.args, { target: { kind: "session", id: "u-1" } });
  assert.equal(shellTabs()[0].querySelector(".tab-title")?.textContent, "Terminal · one");
});

test("Mod+T on a shown shell tab opens another shell in the same folder", async () => {
  await until(() => document.querySelector<HTMLElement>(".terminal[data-tab='ainb-dsh-00000001']:not([hidden])") !== null, "the shell shown");
  await drain();
  chord("KeyT", "T");
  await until(() => shellTabs().length === 2, "a second shell tab");
  assert.deepEqual(lastCall("shell_open")?.args, { target: { kind: "shell", key: "ainb-dsh-00000001" } });
  assert.equal(shellTabs()[1].querySelector(".tab-title")?.textContent, "Terminal · one");
});

test("closing a shell's tab ends the shell, by its x and by Mod+W; a session tab only detaches", async () => {
  const first = shellTabs()[0];
  first.querySelector<HTMLButtonElement>(".tab-close")!.click();
  await until(() => shellTabs().length === 1, "the first shell's tab to close");
  assert.equal(lastCall("shell_close")?.args.key, "ainb-dsh-00000001");
  assert.equal(lastCall("terminal_close"), undefined, "a shell's tab is not merely detached");

  shellTabs()[0].querySelector<HTMLElement>(".tab-title")!.click();
  await until(() => document.querySelector(".terminal[data-tab='ainb-dsh-00000002']:not([hidden])") !== null, "the second shell shown");
  await drain();
  chord("KeyW", "W");
  await until(() => shellTabs().length === 0, "Mod+W to close the second shell");
  assert.equal(lastCall("shell_close")?.args.key, "ainb-dsh-00000002");
  assert.equal(lastCall("terminal_close"), undefined);

  await showTab("u-2");
  chord("KeyW", "W");
  await until(() => lastCall("terminal_close") !== undefined, "the session tab to close");
  assert.equal(lastCall("terminal_close")?.args.key, "tmux_u-2");
});

test("a refusal shows the host's sentence as it came", async () => {
  await showTab("u-1");
  refuse.set("shell_open", MAY_STILL_OPEN_TEXT);
  plus()!.click();
  await until(() => toasts().some((text) => text.includes("may still open")), "the refusal toast");
  assert.ok(toasts().includes(MAY_STILL_OPEN_TEXT), toasts().join(" | "));
  assert.equal(host.tabs.filter((tab) => tab.key.startsWith("ainb-dsh-")).length, 0, "no tab for a refused open");
});
