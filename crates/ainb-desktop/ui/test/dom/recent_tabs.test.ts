// Ctrl+Tab and reopen closed tab in the whole window (`main.tsx`), mounted
// off macOS over the fake host, driven as a person drives them: Ctrl held
// while Tab steps through the focused pane's tabs, most recently shown
// first, Ctrl let go to show one, Esc to back out; a closed tab brought back
// by Ctrl+Alt+Shift+T to the pane and place it closed from.
//
// The fake host answers the tab commands as the real one does: a closed tab
// leaves the strip, a session row opened again brings its tab back focused,
// and `shell_open` opens a new shell tab, focused, in the folder named.
// The tests run in order over one window, each from where the last left it.

import "./window.ts";
import { drain, host, mountWindow, press, showTab, tabOf, until } from "./window_host.ts";

import assert from "node:assert/strict";
import { test } from "node:test";

type Callback = (payload: unknown) => void;
type Target = { kind: "session"; id: string } | { kind: "shell"; key: string };

const internals = (window as unknown as { __TAURI_INTERNALS__: { invoke: (c: string, a?: Record<string, unknown>) => Promise<unknown> } })
  .__TAURI_INTERNALS__;
const hostInvoke = internals.invoke;
/** What the terminals sent their panes. */
const typed: string[] = [];
/** Every `shell_open` target, in order. */
const shellOpens: Target[] = [];
let opened = 0;
/** List a new shell's tab before answering its open, as a host may. */
let listBeforeAnswer = false;
const FOLDERS: Record<string, string> = { "u-1": "/a/one", "u-2": "/a/two", "u-3": "/b/one" };

function emitTabs(focus: string | null): void {
  for (const handler of host.events.get("terminal_tabs") ?? []) {
    (handler as Callback)({ event: "terminal_tabs", id: 0, payload: { tabs: host.tabs, focus } });
  }
}

internals.invoke = async (command, args = {}) => {
  if (command === "terminal_input") typed.push(args.data as string);
  if (command === "shell_open") {
    const target = args.target as Target;
    shellOpens.push(target);
    opened += 1;
    const key = `ainb-dsh-${opened}`;
    const dir =
      target.kind === "session"
        ? FOLDERS[target.id]
        : (host.tabs.find((tab) => tab.key === target.key) as unknown as { target: { dir: string } }).target.dir;
    host.tabs = [...host.tabs, { key, target: { kind: "shell", tmux: key, dir }, state: "attached" } as never];
    if (listBeforeAnswer) {
      emitTabs(key);
      await new Promise((resolve) => setTimeout(resolve, 50));
    } else setTimeout(() => emitTabs(key), 0);
    return key;
  }
  if (command === "terminal_close" || command === "shell_close") {
    host.tabs = host.tabs.filter((tab) => tab.key !== args.key);
    setTimeout(() => emitTabs(null), 0);
    return null;
  }
  return hostInvoke(command, args);
};

/** A tab's short name: a session's id, or a shell's key. */
const short = (key: string) => key.replace(/^tmux_/, "");
/** Each pane as `id:tabs*shown`, `!` on the focused one. */
const panes = () =>
  [...document.querySelectorAll<HTMLElement>(".pane-group")].map((group) => {
    const keys = [...group.querySelectorAll<HTMLElement>(".tab[data-key]")].map((tab) => short(tab.dataset.key!));
    const shown = group.querySelector<HTMLElement>(".tab.active[data-key]")?.dataset.key ?? "";
    return `${group.dataset.group}:${keys.join(",")}*${short(shown)}${group.hasAttribute("data-focused") ? "!" : ""}`;
  });
const switcher = () => document.querySelector(".tab-switcher");
/** The switcher's rows, the chosen one in brackets. */
const rows = () =>
  [...document.querySelectorAll<HTMLElement>(".tab-switcher-option")].map((row) => {
    const name = short(row.dataset.key!);
    return row.getAttribute("aria-selected") === "true" ? `[${name}]` : name;
  });
/** The tab the focused pane shows, while the panes are on screen. */
const shown = () => {
  if (document.querySelector(".panes[hidden]") !== null) return "";
  return short(document.querySelector<HTMLElement>(".pane-group[data-focused] .tab.active[data-key]")?.dataset.key ?? "");
};

const ctrlTab = (shift = false) => press({ code: "Tab", key: "Tab", ctrlKey: true, shiftKey: shift });
/** Let go of Ctrl, where the keyboard is. */
function releaseCtrl(): void {
  const event = new window.KeyboardEvent("keyup", { code: "ControlLeft", key: "Control", bubbles: true, cancelable: true });
  (document.activeElement ?? document.body).dispatchEvent(event);
}
const escape = () => press({ code: "Escape", key: "Escape" });
/** Mod+W off macOS. */
const closeShown = () => press({ code: "KeyW", key: "W", ctrlKey: true, shiftKey: true });
/** Reopen closed tab off macOS. */
const reopen = () => press({ code: "KeyT", key: "T", ctrlKey: true, altKey: true, shiftKey: true });

/** Ctrl held, Tab pressed `steps` times, then Ctrl let go; the tab it shows. */
async function switchBy(steps: number, shift = false): Promise<string> {
  for (let n = 0; n < steps; n += 1) ctrlTab(shift);
  releaseCtrl();
  await until(() => switcher() === null, "the switcher to close");
  await drain();
  return shown();
}

test("Ctrl+Tab over the board opens no switcher", async () => {
  host.tabs = [tabOf("u-1"), tabOf("u-2"), tabOf("u-3")];
  await mountWindow();
  ctrlTab();
  await drain();
  assert.equal(switcher(), null, "Orca's switcher runs over the terminals only");
});

test("held Ctrl+Tab lists the pane's tabs most recently shown first, and steps through them", async () => {
  await showTab("u-1");
  await showTab("u-3");
  await showTab("u-2");
  await until(() => document.activeElement?.className === "xterm-helper-textarea", "u-2's terminal has the keyboard");
  ctrlTab();
  await until(() => switcher() !== null, "the switcher");
  assert.deepEqual(rows(), ["u-2", "[u-3]", "u-1"], "the shown tab, then the one shown before it, chosen");
  assert.equal(shown(), "u-2", "nothing changes while Ctrl is held");
  ctrlTab();
  assert.deepEqual(rows(), ["u-2", "u-3", "[u-1]"]);
  ctrlTab();
  assert.deepEqual(rows(), ["[u-2]", "u-3", "u-1"], "wrapping back to the top");
  ctrlTab();
  releaseCtrl();
  await until(() => shown() === "u-3", "u-3 shown on letting go of Ctrl");
  assert.equal(switcher(), null);
  await drain();
  assert.deepEqual(typed, [], "the terminal that had the keyboard sent its pane no Tab");
});

test("one Ctrl+Tab toggles back to the tab shown before", async () => {
  // Shown now: u-3, before it u-2.
  assert.equal(await switchBy(1), "u-2");
  assert.equal(await switchBy(1), "u-3");
});

test("Shift steps back: Ctrl+Shift+Tab chooses the least recently shown", async () => {
  // Recent now: u-3, u-2, u-1.
  ctrlTab(true);
  await until(() => switcher() !== null, "the switcher");
  assert.deepEqual(rows(), ["u-3", "u-2", "[u-1]"]);
  ctrlTab(true);
  assert.deepEqual(rows(), ["u-3", "[u-2]", "u-1"]);
  ctrlTab();
  assert.deepEqual(rows(), ["u-3", "u-2", "[u-1]"], "Tab steps on again from there");
  releaseCtrl();
  await until(() => shown() === "u-1", "u-1 shown");
  await drain();
});

test("Esc closes the switcher with nothing changed, and Ctrl let go after it shows nothing", async () => {
  ctrlTab();
  await until(() => switcher() !== null, "the switcher");
  const esc = escape();
  assert.equal(switcher(), null, "Esc closed it");
  assert.equal(esc.defaultPrevented, true, "the Esc is the switcher's, not the terminal's");
  assert.ok(!typed.join("").includes("\x1b"), "the terminal with the keyboard never saw the Esc");
  releaseCtrl();
  await drain();
  assert.equal(shown(), "u-1", "still the tab shown before");
});

test("the window losing focus closes the switcher with nothing changed", async () => {
  ctrlTab();
  await until(() => switcher() !== null, "the switcher");
  window.dispatchEvent(new window.Event("blur"));
  assert.equal(switcher(), null);
  releaseCtrl();
  await drain();
  assert.equal(shown(), "u-1");
});

test("a palette opened while Ctrl is held keeps the tab shown when Ctrl is let go", async () => {
  ctrlTab();
  await until(() => switcher() !== null, "the switcher");
  press({ code: "KeyJ", key: "J", ctrlKey: true, shiftKey: true });
  await until(() => document.querySelector(".palette") !== null, "the palette");
  releaseCtrl();
  await drain();
  assert.equal(switcher(), null);
  assert.equal(shown(), "u-1", "no tab switched behind the palette");
  press({ code: "KeyJ", key: "J", ctrlKey: true, shiftKey: true });
  await until(() => document.querySelector(".palette") === null, "the palette closed");
  await drain();
});

test("the switcher lists only the focused pane's tabs, and a pane of one opens none", async () => {
  // Split u-1 out to the right: g1 holds u-2, u-3 and g2 u-1, focused.
  press({ code: "KeyD", key: "D", ctrlKey: true, shiftKey: true });
  await until(() => panes().length === 2, "the split");
  assert.deepEqual(panes(), ["g1:u-2,u-3*u-2", "g2:u-1*u-1!"]);
  ctrlTab();
  await drain();
  assert.equal(switcher(), null, "one tab has nowhere to switch to");
  document.querySelector<HTMLElement>('.pane-group[data-group="g1"] .tab[data-key="tmux_u-2"] .tab-title')!.click();
  await until(() => shown() === "u-2", "g1 focused, on u-2");
  await drain();
  ctrlTab();
  await until(() => switcher() !== null, "the switcher");
  assert.deepEqual(rows(), ["u-2", "[u-3]"], "g1's tabs only, though u-1 was shown more recently than u-3");
  releaseCtrl();
  await until(() => shown() === "u-3", "u-3 shown");
  await drain();
});

test("reopen brings a closed session tab back to its pane and place, attached again", async () => {
  // g1: u-2, u-3 (u-3 shown) | g2: u-1. Close u-2, g1's first, from g1.
  document.querySelector<HTMLElement>('.tab[data-key="tmux_u-2"] .tab-title')!.click();
  await until(() => shown() === "u-2", "u-2 shown");
  await drain();
  closeShown();
  await until(() => panes().join() === "g1:u-3*u-3!,g2:u-1*u-1", "u-2 closed");
  // The keyboard goes to the other pane, then the tab comes back.
  document.querySelector<HTMLElement>('.tab[data-key="tmux_u-1"] .tab-title')!.click();
  await until(() => shown() === "u-1", "u-1 shown in g2");
  await drain();
  host.sent.length = 0;
  reopen();
  await until(() => panes().join() === "g1:u-2,u-3*u-2!,g2:u-1*u-1", `u-2 back at g1's front, not ${panes().join()}`);
  assert.deepEqual(
    host.sent.filter((sent) => sent.id === "session_list.select_row" && sent.open === true),
    [{ id: "session_list.select_row", session: "u-2", open: true }],
    "its session-list row opened again: the same session, reattached",
  );
  assert.equal(shown(), "u-2");
});

test("a closed shell comes back as a fresh shell in its worktree, in its old place", async () => {
  // A shell in u-2's worktree joins g1, then u-3 is shown and the shell closed.
  press({ code: "KeyT", key: "T", ctrlKey: true, shiftKey: true });
  await until(() => panes()[0] === "g1:u-2,u-3,ainb-dsh-1*ainb-dsh-1!", `the shell in g1, not ${panes().join()}`);
  assert.deepEqual(shellOpens.at(-1), { kind: "session", id: "u-2" });
  await drain();
  press({ code: "BracketLeft", key: "{", ctrlKey: true, shiftKey: true });
  await until(() => shown() === "u-3", "u-3 shown");
  press({ code: "BracketRight", key: "}", ctrlKey: true, shiftKey: true });
  await until(() => shown() === "ainb-dsh-1", "the shell shown");
  await drain();
  press({ code: "KeyW", key: "W", ctrlKey: true, shiftKey: true });
  await until(() => panes()[0] === "g1:u-2,u-3*u-3!", "the shell closed");
  document.querySelector<HTMLElement>('.tab[data-key="tmux_u-1"] .tab-title')!.click();
  await until(() => shown() === "u-1", "u-1 shown in g2");
  await drain();
  reopen();
  await until(() => shellOpens.length === 2, "a shell asked for");
  assert.deepEqual(shellOpens[1], { kind: "session", id: "u-2" }, "in the closed shell's folder, named by the session whose worktree it is");
  await until(() => panes().join() === "g1:u-2,u-3,ainb-dsh-2*ainb-dsh-2!,g2:u-1*u-1", `the new shell back in g1, not ${panes().join()}`);
});

test("a reopened shell whose tab is listed before the host answers still goes back to its pane", async () => {
  // g1: u-2, u-3, ainb-dsh-2 (shown, focused) | g2: u-1. Close the shell,
  // focus g2, and have the host list the new shell before it answers.
  closeShown();
  await until(() => panes()[0] === "g1:u-2,u-3*u-3!", "the shell closed");
  document.querySelector<HTMLElement>('.tab[data-key="tmux_u-1"] .tab-title')!.click();
  await until(() => shown() === "u-1", "u-1 shown in g2");
  await drain();
  listBeforeAnswer = true;
  try {
    reopen();
    await until(() => panes().join() === "g1:u-2,u-3,ainb-dsh-3*ainb-dsh-3!,g2:u-1*u-1", `the shell back in g1, not ${panes().join()}`);
  } finally {
    listBeforeAnswer = false;
  }
  await until(() => document.activeElement?.closest<HTMLElement>(".terminal[data-tab]")?.dataset.tab === "ainb-dsh-3", "the shell to have the keyboard");
});

test("reopen skips a session gone from the list and brings back the one closed before it", async () => {
  // u-9 has a tab but no row: its session is gone.
  host.tabs = [...host.tabs, tabOf("u-9")];
  emitTabs(null);
  await until(() => document.querySelector('.tab[data-key="tmux_u-9"]') !== null, "u-9's tab");
  // Close u-3, then u-9.
  document.querySelector<HTMLElement>('.tab[data-key="tmux_u-3"] .tab-title')!.click();
  await until(() => shown() === "u-3", "u-3 shown");
  await drain();
  closeShown();
  await until(() => document.querySelector('.tab[data-key="tmux_u-3"]') === null, "u-3 closed");
  document.querySelector<HTMLElement>('.tab[data-key="tmux_u-9"] .tab-title')!.click();
  await until(() => shown() === "u-9", "u-9 shown");
  await drain();
  closeShown();
  await until(() => document.querySelector('.tab[data-key="tmux_u-9"]') === null, "u-9 closed");
  host.sent.length = 0;
  reopen();
  await until(() => document.querySelector('.tab[data-key="tmux_u-3"]') !== null, "u-3 back");
  const opens = host.sent.filter((sent) => sent.id === "session_list.select_row" && sent.open === true);
  assert.deepEqual(opens.map((sent) => sent.session), ["u-3"], "u-9 was never asked for");
});

test("the closed stack holds ten: eleven closed shells, ten come back", async () => {
  await drain();
  const before = shellOpens.length;
  for (let n = 0; n < 11; n += 1) {
    press({ code: "KeyT", key: "T", ctrlKey: true, shiftKey: true });
    await until(() => shellOpens.length === before + n + 1, `shell ${n + 1}`);
    await until(() => shown() === `ainb-dsh-${opened}`, `shell ${n + 1} shown`);
    await drain();
  }
  const opens = shellOpens.length;
  // Close every shell (the one from the last test too), newest first.
  for (let n = 0; n < 12; n += 1) {
    const shell = [...host.tabs].reverse().find((tab) => tab.key.startsWith("ainb-dsh-"));
    document.querySelector<HTMLElement>(`.tab[data-key="${shell!.key}"] .tab-title`)!.click();
    await until(() => shown() === shell!.key, `${shell!.key} shown`);
    await drain();
    closeShown();
    await until(() => document.querySelector(`.tab[data-key="${shell!.key}"]`) === null, `${shell!.key} closed`);
    await drain();
  }
  const shells = () => host.tabs.filter((tab) => tab.key.startsWith("ainb-dsh-")).length;
  for (let n = 0; n < 10; n += 1) {
    reopen();
    await until(() => shells() === n + 1, `reopen ${n + 1}`);
  }
  // The stack is empty now: the two closed first had fallen off.
  reopen();
  reopen();
  await drain();
  assert.equal(shellOpens.length - opens, 10, "ten reopened, no more");
  assert.equal(shells(), 10);
});
