// Terminal zoom in split panes, on the whole window (`main.tsx`) over the fake
// host, mounted as macOS: u-1 and u-2 side by side, as a stored layout
// restores them. A zoom in the focused pane is that pane's alone: its sibling
// is not refit, not resized at the host, sent no byte, and keeps its font.

import "./window.ts";
import { drain, mountWindow, press, pretendMac, until } from "./window_host.ts";

import assert from "node:assert/strict";
import { before, test } from "node:test";
import { FitAddon } from "@xterm/addon-fit";

const FOCUSED = "tmux_u-1";
const SIBLING = "tmux_u-2";

/** Every terminal call the window made, with the pane it named. */
const calls: { command: string; key: unknown }[] = [];

before(async () => {
  localStorage.setItem(
    "ainb.layout",
    JSON.stringify({
      version: 1,
      focused: "g1",
      next: 3,
      root: {
        kind: "split",
        axis: "row",
        ratios: [0.5, 0.5],
        children: [
          { kind: "group", id: "g1", tabs: [FOCUSED], active: FOCUSED },
          { kind: "group", id: "g2", tabs: [SIBLING], active: SIBLING },
        ],
      },
    }),
  );
  const internals = (window as unknown as { __TAURI_INTERNALS__: { invoke(command: string, args?: Record<string, unknown>): unknown } })
    .__TAURI_INTERNALS__;
  const invoke = internals.invoke;
  internals.invoke = (command, args = {}) => {
    if (command.startsWith("terminal_")) calls.push({ command, key: args.key });
    return invoke(command, args);
  };
  pretendMac();
  await mountWindow();
});

const pane = (key: string) => document.querySelector<HTMLElement>(`.terminal[data-tab="${key}"]`)!;

/** The font size xterm's stylesheet for `key`'s pane draws with, in px. */
function fontSize(key: string): number {
  const owner = [...(pane(key).querySelector(".xterm")?.classList ?? [])].find((name) => name.startsWith("xterm-dom-renderer-owner-"));
  assert.ok(owner, `${key} is drawn by xterm's DOM renderer`);
  for (const style of document.querySelectorAll("style")) {
    const text = style.textContent ?? "";
    if (!text.includes(owner)) continue;
    const size = /font-size:\s*(\d+)px/.exec(text);
    if (size) return Number(size[1]);
  }
  assert.fail(`no stylesheet sizes ${key}`);
}

test("zooming the focused pane leaves its sibling unfit, unresized, unsent to, at its own size", async () => {
  document.querySelector<HTMLElement>(".terminals-tab .tab-title")!.click();
  await until(() => document.querySelectorAll(".terminal[data-tab]:not([hidden])").length === 2, "both panes' terminals");
  await drain();
  // happy-dom lays nothing out, so every element reads as hidden: both panes
  // are shown, so a refit of either one would really run.
  for (const key of [FOCUSED, SIBLING]) {
    Object.defineProperty(pane(key).querySelector(".xterm-host"), "offsetParent", { get: () => document.body });
  }
  const fit = FitAddon.prototype.fit;
  const fits: string[] = [];
  FitAddon.prototype.fit = function (this: FitAddon) {
    const element = (this as unknown as { _terminal?: { element?: HTMLElement } })._terminal?.element;
    fits.push(element?.closest<HTMLElement>(".terminal[data-tab]")?.dataset.tab ?? "unknown");
    return fit.call(this);
  };
  try {
    assert.equal(fontSize(FOCUSED), 13);
    assert.equal(fontSize(SIBLING), 13);
    pane(FOCUSED).querySelector<HTMLTextAreaElement>(".xterm-helper-textarea")!.focus();
    calls.length = 0;
    press({ code: "Equal", key: "=", metaKey: true });
    await drain();

    assert.equal(fontSize(FOCUSED), 14, "the focused pane zoomed");
    assert.deepEqual(fits, [FOCUSED], "only the focused pane refit");
    assert.deepEqual(calls, [{ command: "terminal_resize", key: FOCUSED }], "only the focused pane's size reached the host, and no byte for either");
    assert.equal(fontSize(SIBLING), 13, "the sibling keeps its font");
  } finally {
    FitAddon.prototype.fit = fit;
  }
});
