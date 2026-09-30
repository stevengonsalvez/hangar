// "New terminal" in the whole window (`main.tsx`), mounted over the fake host:
// the pane strip's + menu and Mod+T ask the host for a shell in the shown
// tab's worktree, the host's tab lands in the strip, and closing a shell's tab,
// by its x or by Mod+W, ends the shell (`shell_close`) where any other tab
// only detaches (`terminal_close`). A refusal shows the host's sentence.
//
// The fake host answers `shell_open` as the real one does: a new shell tab,
// focused, in the folder the target names.
//
// The window mounts once per file, so each test starts from the same place
// (`beforeEach`): no shell tabs, no recorded calls, no queued refusal. Any one
// test runs alone (`--test-name-pattern`) as it runs in the file.

import "./window.ts";
import { drain, host, mountWindow, press, showTab, tabOf, until } from "./window_host.ts";

import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";

/** The host's sentence for an open that may have made its shell
 * (`create::MAY_STILL_OPEN` and the daemon's detail). */
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
    const key = `ainb-dsh-${String(opened).padStart(8, "0")}`;
    const dir = folderOf(args.target as Target);
    host.tabs = [...host.tabs, { key, target: { kind: "shell", tmux: key, dir }, state: "attached" } as never];
    setTimeout(() => emitTabs(key), 0);
    return key;
  }
  host.tabs = host.tabs.filter((tab) => tab.key !== args.key);
  setTimeout(() => emitTabs(null), 0);
  return null;
};

const plus = () => document.querySelector<HTMLButtonElement>(".pane-strip .tab-new");
/** Open the pane's + menu and pick New terminal, as a person does. */
async function newTerminal(): Promise<void> {
  plus()!.click();
  await until(() => document.querySelector(".tab-create-menu") !== null, "the + menu");
  document.querySelector<HTMLButtonElement>('.tab-create-menu [data-item="terminal"]')!.click();
}
const lastCall = (command: string) => [...calls].reverse().find((call) => call.command === command);
const shellTabs = () =>
  [...document.querySelectorAll<HTMLElement>(".tab[data-state]")].filter((tab) =>
    tab.querySelector(".tab-title")?.textContent?.startsWith("Terminal"),
  );
const toasts = () => [...document.querySelectorAll(".toast")].map((toast) => toast.textContent ?? "");
/** Mod+T and Mod+W as this window reads them off macOS: Ctrl+Shift. */
const chord = (code: string, key: string) => press({ code, key, ctrlKey: true, shiftKey: true });
const isShell = (key: string) => key.startsWith("ainb-dsh-");

let mounted: Promise<void> | undefined;

beforeEach(async () => {
  mounted ??= mountWindow();
  await mounted;
  calls.length = 0;
  refuse.clear();
  // The strip the window mounted with: the shells an earlier test opened
  // end, and a session tab it closed is back.
  const start = [tabOf("u-1"), tabOf("u-2")];
  if (host.tabs.map((tab) => tab.key).join() !== start.map((tab) => tab.key).join()) {
    host.tabs = start;
    emitTabs(null);
  }
  await until(() => document.querySelectorAll(".tab[data-state]").length === start.length, "the starting strip");
});

/** Open a shell from `sessionId`'s tab, shown, and answer its key. */
async function openShellFrom(sessionId: string): Promise<string> {
  await showTab(sessionId);
  const before = shellTabs().length;
  await newTerminal();
  await until(() => shellTabs().length === before + 1, "the shell's tab in the strip");
  const key = host.tabs[host.tabs.length - 1].key;
  await until(() => document.querySelector(`.terminal[data-tab='${key}']:not([hidden])`) !== null, "the shell shown");
  await drain();
  return key;
}

test("the pane strip's + opens a shell in the shown tab's worktree", async () => {
  assert.ok(plus(), "the pane strip mounts the + menu");
  assert.equal(document.querySelector("nav.tabs:not(.pane-strip) .tab-new"), null, "none beside Board and Review");

  await openShellFrom("u-1");
  // Only ids cross: the host resolves the folder.
  assert.deepEqual(lastCall("shell_open")?.args, { target: { kind: "session", id: "u-1" } });
  assert.equal(shellTabs()[0].querySelector(".tab-title")?.textContent, "Terminal · one");
});

test("Mod+T on a shown shell tab opens another shell in the same folder", async () => {
  const first = await openShellFrom("u-1");
  chord("KeyT", "T");
  await until(() => shellTabs().length === 2, "a second shell tab");
  assert.deepEqual(lastCall("shell_open")?.args, { target: { kind: "shell", key: first } });
  assert.equal(shellTabs()[1].querySelector(".tab-title")?.textContent, "Terminal · one");
});

test("closing a shell's tab ends the shell, by its x and by Mod+W; a session tab only detaches", async () => {
  const first = await openShellFrom("u-1");
  const second = await openShellFrom("u-1");
  shellTabs()[0].querySelector<HTMLButtonElement>(".tab-close")!.click();
  await until(() => shellTabs().length === 1, "the first shell's tab to close");
  assert.equal(lastCall("shell_close")?.args.key, first);
  assert.equal(lastCall("terminal_close"), undefined, "a shell's tab is not merely detached");

  shellTabs()[0].querySelector<HTMLElement>(".tab-title")!.click();
  await until(() => document.querySelector(`.terminal[data-tab='${second}']:not([hidden])`) !== null, "the second shell shown");
  await drain();
  chord("KeyW", "W");
  await until(() => shellTabs().length === 0, "Mod+W to close the second shell");
  assert.equal(lastCall("shell_close")?.args.key, second);
  assert.equal(lastCall("terminal_close"), undefined);

  await showTab("u-2");
  chord("KeyW", "W");
  await until(() => lastCall("terminal_close") !== undefined, "the session tab to close");
  assert.equal(lastCall("terminal_close")?.args.key, "tmux_u-2");
});

test("a refusal shows the host's sentence as it came", async () => {
  await showTab("u-1");
  refuse.set("shell_open", MAY_STILL_OPEN_TEXT);
  await newTerminal();
  await until(() => toasts().some((text) => text.includes("may still open")), "the refusal toast");
  assert.ok(toasts().includes(MAY_STILL_OPEN_TEXT), toasts().join(" | "));
  assert.equal(host.tabs.filter((tab) => isShell(tab.key)).length, 0, "no tab for a refused open");
});

test("a long refusal is cleaned and cut at a toast's cap, not a label's", async () => {
  await showTab("u-1");
  // The page cuts a refusal at TOAST_CHARS (300, held equal to
  // intent::MAX_TOAST_CHARS); a bidi override or an escape in its detail
  // must not restyle the toast.
  const detail = "d".repeat(400);
  refuse.set("shell_open", `\u202EOpening the terminal failed:\u001b ${detail}`);
  await newTerminal();
  await until(() => toasts().some((text) => text.startsWith("Opening the terminal failed")), "the refusal toast");
  const shown = toasts().find((text) => text.startsWith("Opening the terminal failed"))!;
  assert.equal(Array.from(shown).length, 300, "cut at the toast's cap");
  assert.ok(shown.startsWith("Opening the terminal failed: ddd"), shown.slice(0, 40));
});
