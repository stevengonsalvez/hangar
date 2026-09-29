// "New terminal" mounted for real: the strip's + asks the host for a shell in
// the selected session's worktree, the host's tab lands on the strip, and
// closing that tab ends the shell. Both go through the same `invoke` stub the
// window calls through; the strip is the window's own `TerminalTab`.

import { hostCalls, hostReplies, settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent, createSignal, For } from "solid-js";
import { render } from "solid-js/web";
import { createShellTabs, NO_SESSION, shellTitle } from "../../src/shell_tab.ts";
import { NewTerminalButton } from "../../src/shell_tab.tsx";
import type { Tab } from "../../src/tabs.ts";
import { TerminalTab } from "../../src/terminal_tab.tsx";

const toasts: string[] = [];
const [selected, setSelected] = createSignal<string | null>("s-1");
/** The strip as the host's `terminal_tabs` event leaves it. */
const [strip, setStrip] = createSignal<Tab[]>([]);

const SHELL: Tab = {
  key: "ainb-dsh-0123abcd",
  target: { kind: "shell", tmux: "ainb-dsh-0123abcd", dir: "/code/app/.worktrees/feat" },
  state: "attached",
};

function Harness() {
  const shells = createShellTabs({ session: selected, toast: (message) => toasts.push(message) });
  return [
    createComponent(For, {
      get each() {
        return strip();
      },
      children: (tab: Tab) =>
        createComponent(TerminalTab, {
          tab,
          title: tab.target.kind === "shell" ? shellTitle(tab.target) : tab.key,
          active: false,
          status: null,
          onChoose() {},
          onClose: () => shells.close(tab),
        }),
    }),
    createComponent(NewTerminalButton, {
      get ready() {
        return selected() !== null;
      },
      mac: true,
      onOpen: () => void shells.open(),
    }),
  ];
}

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  document.body.innerHTML = "";
  hostReplies.clear();
  hostCalls.clear();
  toasts.length = 0;
  setSelected("s-1");
  setStrip([]);
});

async function mount() {
  const container = document.createElement("div");
  document.body.appendChild(container);
  cleanup = render(Harness, container);
  await settle();
  return container;
}

const plus = (container: HTMLElement) => container.querySelector<HTMLButtonElement>(".tab-new-terminal")!;

test("the + opens a shell in the selected session's worktree and its tab lands", async () => {
  hostReplies.set("shell_open", SHELL.key);
  const container = await mount();
  assert.equal(plus(container).title, "New terminal (⌘T)");
  plus(container).click();
  await settle();
  // Only the session id crosses: the host resolves the folder itself.
  assert.deepEqual(hostCalls.get("shell_open"), { session: "s-1" });
  // The host answers with a tab on the strip, as `terminal_tabs` does.
  setStrip([SHELL]);
  await settle();
  const title = container.querySelector(".tab .tab-title")?.textContent;
  assert.equal(title, "Terminal · feat");
  assert.deepEqual(toasts, []);
});

test("closing a shell's tab ends the shell; any other tab only detaches", async () => {
  const container = await mount();
  setStrip([{ key: "tmux_app", target: { kind: "session", id: "s-1", tmux: "tmux_app" }, state: "attached" }, SHELL]);
  await settle();
  const closes = container.querySelectorAll<HTMLButtonElement>(".tab-close");
  closes[1].click();
  await settle();
  assert.deepEqual(hostCalls.get("shell_close"), { key: SHELL.key });
  assert.equal(hostCalls.has("terminal_close"), false, "a shell tab is not merely detached");
  closes[0].click();
  await settle();
  assert.deepEqual(hostCalls.get("terminal_close"), { key: "tmux_app" });
});

test("a refusal shows the host's sentence, with the daemon's detail, as it came", async () => {
  const refusal =
    "Opening the terminal failed: this worktree's repository is not in a registered project folder. " +
    "Use Add project to pick its folder, then try again. (worktree_path is neither a registered repository)";
  hostReplies.set("shell_open", new Error(refusal));
  const container = await mount();
  plus(container).click();
  await settle();
  assert.deepEqual(toasts, [refusal]);

  hostReplies.set("shell_close", new Error("Closing the terminal failed: tmux could not close ainb-dsh-0123abcd"));
  setStrip([SHELL]);
  await settle();
  container.querySelector<HTMLButtonElement>(".tab-close")!.click();
  await settle();
  assert.equal(toasts[1], "Closing the terminal failed: tmux could not close ainb-dsh-0123abcd");
});

test("with no session selected the + is off and says why", async () => {
  setSelected(null);
  const container = await mount();
  assert.equal(plus(container).disabled, true);
  assert.equal(plus(container).title, NO_SESSION);
  // Mod+T reaches `open` without the button: it says why instead of asking.
  const shells = createShellTabs({ session: () => null, toast: (message) => toasts.push(message) });
  await shells.open();
  assert.deepEqual(toasts, [NO_SESSION]);
  assert.equal(hostCalls.has("shell_open"), false);
});
