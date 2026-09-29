// The whole window over the fake host of `tab_host.ts`, launched the way a
// real relaunch is: the host restores no tabs, so its first strip is empty,
// and the stored layout must wait for the first strip that has one. Then the
// guarantees the panes rest on: a terminal is one node for its tab's whole
// life, whatever the layout does around it, and a pane whose shown tab
// closes goes back to the tab it showed before.

import { fromMenu, host, measure, panes, pointer, strip, tab, tabEl, tick, until, visible } from "./tab_host.ts";

import assert from "node:assert/strict";
import { test } from "node:test";

/** Stored last time: a and c on the left, b on the right, focused. */
const SAVED = JSON.stringify({
  version: 1,
  focused: "g2",
  next: 3,
  root: {
    kind: "split",
    axis: "row",
    ratios: [0.5, 0.5],
    children: [
      { kind: "group", id: "g1", tabs: ["tmux_a", "tmux_c"], active: "tmux_a" },
      { kind: "group", id: "g2", tabs: ["tmux_b"], active: "tmux_b" },
    ],
  },
});

test("a relaunch whose host lists no tab yet restores the stored panes from the first strip that has one", async () => {
  localStorage.setItem("ainb.layout", SAVED);
  host.tabs = [];
  const root = document.createElement("div");
  root.id = "root";
  document.body.appendChild(root);
  await import("../../src/main.tsx");
  await until(() => host.calls.some((call) => call.command === "terminal_tabs"), "the window asking for the strip");
  await tick();
  assert.equal(localStorage.getItem("ainb.layout"), SAVED, "an empty strip leaves the stored layout alone");

  host.tabs = ["a", "b", "c"].map(tab);
  strip();
  await until(() => document.querySelectorAll(".pane-group").length === 2, "the stored panes");
  assert.deepEqual(panes(), ["g1:a,c*a", "g2:b*b!"]);
});

test("a terminal is one node for its tab's life: through a split, a move, a drag and host frames", async () => {
  document.querySelector<HTMLElement>(".terminals-tab .tab-title")!.click();
  await until(() => visible().length === 2, "both panes' terminals");
  const nodes = () => [...document.querySelectorAll<HTMLElement>('.terminal[data-tab="tmux_c"]')];
  const [first] = nodes();
  const same = (after: string) => {
    const now = nodes();
    assert.equal(now.length, 1, `one terminal for c after ${after}`);
    assert.equal(now[0], first, `c's terminal is the same node after ${after}`);
  };
  assert.equal(nodes().length, 1);

  await fromMenu("c", '[data-split="down"]');
  assert.deepEqual(panes(), ["g1:a*a", "g3:c*c!", "g2:b*b"]);
  same("a split");

  await fromMenu("c", '[data-move="g2"]');
  assert.deepEqual(panes(), ["g1:a*a", "g2:b,c*c!"]);
  same("a move");

  // Drag c from the right pane's strip to the left pane's right edge.
  measure();
  pointer(tabEl("c").querySelector(".tab-title")!, "pointerdown", 700, 10);
  pointer(window, "pointermove", 250, 300);
  pointer(window, "pointerup", 490, 300);
  await tick();
  assert.deepEqual(panes(), ["g1:a*a", "g4:c*c!", "g2:b*b"]);
  same("a drag");

  strip("tmux_c");
  await tick();
  same("a host frame that focuses it");
  host.tabs = [...host.tabs, tab("d")];
  strip();
  await until(() => tabEl("d") !== null, "d in a strip");
  same("a host frame that opens another tab");
});

test("a pane whose shown tab closes shows the tab it showed before, not the neighbour", async () => {
  // x, y and z join the focused pane; they are shown z, then x, then y.
  host.tabs = [...host.tabs, tab("x"), tab("y"), tab("z")];
  strip();
  await until(() => tabEl("z") !== null, "x, y and z in a strip");
  for (const name of ["z", "x", "y"]) {
    tabEl(name).querySelector<HTMLElement>(".tab-title")!.click();
    await until(() => panes().some((pane) => pane.endsWith(`*${name}!`)), `${name} shown`);
  }
  tabEl("y").querySelector<HTMLElement>(".tab-close")!.click();
  await until(() => tabEl("y") === null, "y closed");
  const pane = panes().find((one) => one.includes(",x") || one.includes(":x"));
  assert.ok(pane?.endsWith("*x!"), `the pane shows x, shown before y, not z beside it: ${pane}`);
  assert.ok(visible().includes("x") && !visible().includes("z"));
});
