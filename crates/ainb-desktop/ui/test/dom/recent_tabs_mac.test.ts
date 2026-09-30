// Ctrl+Tab and Cmd+Shift+T on macOS, in the whole window over the fake host
// mounted as macOS: Orca binds both the same there
// (`orca:src/shared/keybindings/definitions-core-2.ts:158-165,199-207`).

import "./window.ts";
import { drain, host, mountWindow, press, pretendMac, showTab, shownTerminal, until } from "./window_host.ts";

import assert from "node:assert/strict";
import { before, test } from "node:test";

type Callback = (payload: unknown) => void;
const internals = (window as unknown as { __TAURI_INTERNALS__: { invoke: (c: string, a?: Record<string, unknown>) => Promise<unknown> } })
  .__TAURI_INTERNALS__;
const hostInvoke = internals.invoke;
internals.invoke = async (command, args = {}) => {
  if (command !== "terminal_close") return hostInvoke(command, args);
  host.tabs = host.tabs.filter((tab) => tab.key !== args.key);
  setTimeout(() => {
    for (const handler of host.events.get("terminal_tabs") ?? []) {
      (handler as Callback)({ event: "terminal_tabs", id: 0, payload: { tabs: host.tabs, focus: null } });
    }
  }, 0);
  return null;
};

before(async () => {
  pretendMac();
  await mountWindow();
});

test("Ctrl+Tab, not Cmd+Tab, switches to the tab shown before on letting go of Ctrl", async () => {
  await showTab("u-1");
  await showTab("u-2");
  press({ code: "Tab", key: "Tab", metaKey: true });
  await drain();
  assert.equal(document.querySelector(".tab-switcher"), null, "Cmd+Tab is the system's app switcher");
  press({ code: "Tab", key: "Tab", ctrlKey: true });
  await until(() => document.querySelector(".tab-switcher") !== null, "the switcher");
  (document.activeElement ?? document.body).dispatchEvent(
    new window.KeyboardEvent("keyup", { code: "ControlLeft", key: "Control", bubbles: true, cancelable: true }),
  );
  await until(() => shownTerminal() === "tmux_u-1", "u-1 shown");
});

test("Cmd+Shift+T reopens the tab Cmd+W closed", async () => {
  await drain();
  press({ code: "KeyW", key: "w", metaKey: true });
  await until(() => document.querySelector('.tab[data-key="tmux_u-1"]') === null, "u-1 closed");
  press({ code: "KeyT", key: "t", metaKey: true, shiftKey: true });
  await until(() => document.querySelector('.tab[data-key="tmux_u-1"]') !== null, "u-1 back");
  await until(() => shownTerminal() === "tmux_u-1", "u-1 shown");
});
