// Cmd+D in the whole window (`main.tsx`), mounted as macOS over the fake
// host: a pane of several tabs splits its shown tab out, as before, and a
// pane of one tab splits too, as Orca's does, by opening a shell in that
// pane's worktree (`shell_open`) and splitting the new tab out. The host
// answers `shell_open` and sends its strip in either order, so both orders
// are driven. A refused open shows the host's sentence and splits nothing.

import "./window.ts";
import { drain, host, mountWindow, press, pretendMac, showTab, until } from "./window_host.ts";

import assert from "node:assert/strict";
import { test } from "node:test";

type Callback = (payload: unknown) => void;

const internals = (window as unknown as { __TAURI_INTERNALS__: { invoke: (c: string, a?: Record<string, unknown>) => Promise<unknown> } })
  .__TAURI_INTERNALS__;
const hostInvoke = internals.invoke;
const opens: Array<Record<string, unknown>> = [];
/** How the fake host answers the next `shell_open`: the strip after its
 * answer, the strip before it, or a refusal with this sentence. */
let answer: "strip-after" | "strip-before" | { refuse: string } = "strip-after";

function emitTabs(focus: string | null): void {
  for (const handler of host.events.get("terminal_tabs") ?? []) {
    (handler as Callback)({ event: "terminal_tabs", id: 0, payload: { tabs: host.tabs, focus } });
  }
}

internals.invoke = async (command, args = {}) => {
  if (command !== "shell_open") return hostInvoke(command, args);
  opens.push(args);
  if (typeof answer === "object") throw answer.refuse;
  const key = `ainb-dsh-0000000${opens.length}`;
  host.tabs = [...host.tabs, { key, target: { kind: "shell", tmux: key, dir: "/a/two" }, state: "attached" } as never];
  if (answer === "strip-before") {
    emitTabs(key);
    await new Promise((resolve) => setTimeout(resolve, 20));
  } else {
    setTimeout(() => emitTabs(key), 20);
  }
  return key;
};

/** Each pane as `tabs*shown`, in drawing order, `!` on the focused one;
 * session tabs by their id, shell tabs as `sh<n>`. */
function panes(): string[] {
  const name = (key: string | undefined) => (key?.startsWith("ainb-dsh-") ? `sh${Number(key.slice(9))}` : key?.slice(5) ?? "");
  return [...document.querySelectorAll<HTMLElement>(".pane-group")].map((group) => {
    const keys = [...group.querySelectorAll<HTMLElement>(".tab[data-key]")].map((tab) => name(tab.dataset.key));
    const shown = name(group.querySelector<HTMLElement>(".tab.active[data-key]")?.dataset.key);
    return `${keys.join(",")}*${shown}${group.hasAttribute("data-focused") ? "!" : ""}`;
  });
}
const cmdD = () => press({ code: "KeyD", key: "d", metaKey: true });
const cmdShiftD = () => press({ code: "KeyD", key: "D", metaKey: true, shiftKey: true });
/** How many seams split panes side by side (`row`) and one over another
 * (`column`). */
const seams = (axis: "row" | "column") => document.querySelectorAll(`.pane-divider[data-axis="${axis}"]`).length;
const toasts = () => [...document.querySelectorAll(".toast")].map((toast) => toast.textContent ?? "");

pretendMac();

test("Cmd+D on a pane of several tabs splits its shown tab out, opening nothing", async () => {
  await mountWindow();
  await showTab("u-2");
  assert.deepEqual(panes(), ["u-1,u-2*u-2!"]);
  cmdD();
  await until(() => panes().length === 2, "the split");
  await drain();
  assert.deepEqual(panes(), ["u-1*u-1", "u-2*u-2!"]);
  assert.equal(opens.length, 0, "a pane with a tab to split out opens no shell");
});

test("Cmd+D on a pane of one tab opens a shell in its worktree and splits it out", async () => {
  cmdD();
  await until(() => panes().length === 3, "the one-tab pane to split");
  assert.deepEqual(opens, [{ target: { kind: "session", id: "u-2" } }], "the shell opens in the pane's worktree, named by id");
  assert.deepEqual(panes(), ["u-1*u-1", "u-2*u-2", "sh1*sh1!"], "two panes of one tab each where there was one");
  assert.ok(!toasts().some((text) => text.includes("to split it")), toasts().join(" | "));
});

test("the split lands when the host sends its strip before it answers", async () => {
  answer = "strip-before";
  cmdD();
  await until(() => panes().length === 4, "the new shell's pane to split");
  assert.deepEqual(panes(), ["u-1*u-1", "u-2*u-2", "sh1*sh1", "sh2*sh2!"]);
});

test("a refused open shows the host's sentence and splits nothing", async () => {
  const refusal = "Opening the terminal failed: the daemon refused it: no tmux";
  answer = { refuse: refusal };
  cmdD();
  await until(() => toasts().includes(refusal), "the refusal toast");
  await drain();
  assert.equal(opens.length, 3);
  assert.deepEqual(panes(), ["u-1*u-1", "u-2*u-2", "sh1*sh1", "sh2*sh2!"], "no pane and no tab for a refused open");
});

test("Cmd+Shift+D on a pane of one tab splits the new shell down, not right", async () => {
  answer = "strip-after";
  const [rows, columns] = [seams("row"), seams("column")];
  cmdShiftD();
  await until(() => panes().length === 5, "the one-tab pane to split down");
  assert.deepEqual(opens.at(-1), { target: { kind: "shell", key: "ainb-dsh-00000002" } }, "the shell opens in the focused shell's worktree");
  assert.deepEqual(panes(), ["u-1*u-1", "u-2*u-2", "sh1*sh1", "sh2*sh2", "sh4*sh4!"]);
  assert.equal(seams("column"), columns + 1, "the new pane sits under the old one");
  assert.equal(seams("row"), rows, "no pane was put beside another");
});
