// The whole window, mounted over a fake host that owns the tab strip as the
// real one does (`tab_host.ts`), driven the way a person drives the split
// panes: a relaunch over a stored layout, the host focusing a tab, the tab
// menu, the split chords, a seam dragged, a tab dragged, a pane closed. Each
// test reads what the window drew: which tabs each pane's strip holds, which
// terminals show, and what went to the host.
//
// The tests run in order over one window, each starting from where the last
// left it, as `settings_tab.test.ts` does: the window renders at import.

import {
  closes,
  fromMenu,
  group,
  host,
  measure,
  panes,
  pointer,
  selected,
  stored,
  strip,
  tab,
  tabEl,
  tick,
  until,
  visible,
} from "./tab_host.ts";

import assert from "node:assert/strict";
import { test } from "node:test";

test("a relaunch restores the stored panes, following the tabs the host has live", async () => {
  // Stored last time: a | b, the right pane squeezed to nothing and
  // focused, with a tab since closed. The host now lists a, b, c and d.
  localStorage.setItem(
    "ainb.layout",
    JSON.stringify({
      version: 1,
      focused: "g2",
      next: 3,
      root: {
        kind: "split",
        axis: "row",
        ratios: [1, 5e-324],
        children: [
          { kind: "group", id: "g1", tabs: ["tmux_a", "tmux_gone"], active: "tmux_a" },
          { kind: "group", id: "g2", tabs: ["tmux_b"], active: "tmux_b" },
        ],
      },
    }),
  );
  const root = document.createElement("div");
  root.id = "root";
  document.body.appendChild(root);
  await import("../../src/main.tsx");
  await until(() => document.querySelectorAll(".pane-group").length === 2, "the two stored panes");

  assert.deepEqual(panes(), ["g1:a*a", "g2:b,c,d*b!"], "the gone tab dropped out; new ones joined the focused pane");
  const width = parseFloat(group("g2").style.width);
  assert.ok(Math.abs(width - 15) < 1e-9, `a stored sliver loads at the minimum share, not ${width}%`);
  // tmux_gone may yet reopen: until it does, a person reshapes the panes, or
  // the restore times out, the stored layout is kept as it was. The menu
  // split in a later test ends it, and stores the split.
  assert.deepEqual(stored().root.children[0].tabs, ["tmux_a", "tmux_gone"], "the stored layout waits for the tab still missing");
});

test("the panes show one terminal each, side by side", async () => {
  document.querySelector<HTMLElement>(".terminals-tab .tab-title")!.click();
  await until(() => visible().length === 2, "both panes' terminals");
  assert.deepEqual(visible(), ["a", "b"]);
  const slot = (name: string) => document.querySelector<HTMLElement>(`.pane-slot[data-key="tmux_${name}"]`)!;
  assert.equal(slot("a").style.left, "0%");
  assert.ok(Math.abs(parseFloat(slot("b").style.left) - 85) < 1e-9, "the right pane's terminal sits over the right pane");
  assert.ok(document.querySelector(".tabs:not(.pane-strip) .tab[data-key]") === null, "no terminal tab is left in the top strip");
});

test("the host's focus shows its tab in that tab's pane and focuses the pane", async () => {
  host.calls = [];
  strip("tmux_c");
  await until(() => panes().includes("g2:b,c,d*c!"), "c shown in its pane");
  assert.deepEqual(visible(), ["a", "c"]);
  await until(() => selected().includes("u-c"), "the sidebar following the shown session");
});

test("a tab's menu splits it out beside its pane", async () => {
  await fromMenu("d", '[data-split="right"]');
  assert.deepEqual(panes(), ["g1:a*a", "g2:b,c*c", "g3:d*d!"]);
  assert.deepEqual(visible(), ["a", "c", "d"]);
  assert.deepEqual(
    stored().root.children.map((child: { id: string }) => child.id),
    ["g1", "g2", "g3"],
    "the split is stored",
  );
});

test("a tab's menu moves it to another pane, and the pane it empties collapses", async () => {
  await fromMenu("d", '[data-move="g1"]');
  assert.deepEqual(panes(), ["g1:a,d*d!", "g2:b,c*c"]);
  assert.equal(document.querySelector('.pane-group[data-group="g3"]'), null);
});

test("Ctrl+Shift+D splits the focused pane right, Alt+Shift+D down, as Orca's keys do", async () => {
  const press = (init: KeyboardEventInit) =>
    window.dispatchEvent(new window.KeyboardEvent("keydown", { code: "KeyD", key: "D", bubbles: true, cancelable: true, ...init }));
  press({ ctrlKey: true, shiftKey: true });
  await tick();
  assert.deepEqual(panes(), ["g1:a*a", "g4:d*d!", "g2:b,c*c"]);
  // A pane of one tab has nothing to split out: nothing changes, and it says so.
  press({ altKey: true, shiftKey: true });
  await tick();
  assert.deepEqual(panes(), ["g1:a*a", "g4:d*d!", "g2:b,c*c"]);
  assert.ok([...document.querySelectorAll(".toast")].some((toast) => toast.textContent?.includes("to split it")));
  // Down, from a pane of two.
  tabEl("b").querySelector<HTMLElement>(".tab-title")!.click();
  await until(() => panes().includes("g2:b,c*b!"), "b chosen");
  press({ altKey: true, shiftKey: true });
  await tick();
  assert.deepEqual(panes(), ["g1:a*a", "g4:d*d", "g2:c*c", "g5:b*b!"]);
});

test("a seam dragged past the edge stops at the minimum share", async () => {
  measure();
  // The column g2 over g5 is the last of the root row: drag the seam between
  // them to the very top.
  const seam = document.querySelector<HTMLElement>('.pane-divider[data-axis="column"]')!;
  assert.ok(seam, "the stacked panes have a seam");
  pointer(seam, "pointerdown", 900, 300);
  pointer(window, "pointermove", 900, 10);
  pointer(window, "pointerup", 900, -50);
  await tick();
  const top = parseFloat(group("g2").style.height);
  const whole = top + parseFloat(group("g5").style.height);
  assert.ok(Math.abs(top / whole - 0.15) < 1e-9, `g2 keeps 15% of the column, not ${top / whole}`);
});

test("a tab dragged onto another pane's edge splits that pane with it", async () => {
  measure();
  // g1 is the left 35% or so; drag c from its strip to g1's bottom edge.
  const left = parseFloat(group("g1").style.width) / 100;
  const title = tabEl("c").querySelector<HTMLElement>(".tab-title")!;
  pointer(title, "pointerdown", 700, 10);
  pointer(window, "pointermove", 400, 300);
  assert.ok(document.querySelector(".pane-drop"), "the drop is previewed");
  pointer(window, "pointerup", left * 1000 * 0.5, 590);
  await tick();
  assert.equal(document.querySelector(".pane-drop"), null);
  assert.deepEqual(panes(), ["g1:a*a", "g6:c*c!", "g4:d*d", "g5:b*b"], "c split out under g1; g2, emptied, collapsed");
});

test("closing a pane's last tab collapses the pane", async () => {
  assert.deepEqual(group("g4") && panes().find((pane) => pane.startsWith("g4:")), "g4:d*d", "d is alone in its pane");
  host.calls = [];
  tabEl("d").querySelector<HTMLElement>(".tab-close")!.click();
  await until(() => document.querySelector('.pane-group[data-group="g4"]') === null, "g4 to collapse");
  assert.deepEqual(closes(), ["d"]);
  assert.ok(!panes().some((pane) => pane.includes("d")));
});

test("Close split pane closes every terminal of the pane, and only those", async () => {
  // Gather c and b into one pane first: a | c,b.
  await fromMenu("b", '[data-move="g6"]');
  assert.deepEqual(panes(), ["g1:a*a", "g6:c,b*b!"]);
  host.calls = [];
  document.querySelector<HTMLElement>('.pane-group[data-group="g6"] .pane-actions')!.click();
  await until(() => document.querySelector(".pane-menu [data-close-group]") !== null, "the pane actions menu");
  document.querySelector<HTMLElement>(".pane-menu [data-close-group]")!.click();
  await until(() => document.querySelectorAll(".pane-group").length === 1, "the pane to collapse");
  assert.deepEqual(closes().sort(), ["b", "c"]);
  assert.deepEqual(panes(), ["g1:a*a!"]);
});

test("a paste sizes its pane first, so the host answers it for the pane in front", async () => {
  // Two panes again, a and b side by side, each sized by its own observer:
  // the host answers a paste only for the pane it last sized.
  host.tabs = [...host.tabs, tab("b")];
  strip("tmux_b");
  await until(() => panes().length === 1 && panes()[0].includes("b"), "b opened");
  await fromMenu("b", '[data-split="right"]');
  host.calls = [];
  // The other pane's observer sized it last, as a window resize would.
  host.sized = "tmux_a";
  const textarea = document.querySelector<HTMLTextAreaElement>('.terminal[data-tab="tmux_b"] .xterm-helper-textarea')!;
  textarea.dispatchEvent(new window.KeyboardEvent("keydown", { code: "KeyV", key: "V", ctrlKey: true, shiftKey: true, bubbles: true, cancelable: true }));
  await until(() => host.calls.some((call) => call.command === "clipboard_read"), "the paste's read");
  const order = host.calls.filter((call) => call.command === "terminal_resize" || call.command === "clipboard_read");
  const read = order.findIndex((call) => call.command === "clipboard_read");
  assert.ok(read > 0, "a resize went before the read");
  assert.deepEqual(order[read - 1], { command: "terminal_resize", args: order[read - 1].args });
  assert.equal(order[read - 1].args.key, "tmux_b");
  assert.equal(order[read].args.key, "tmux_b");
  await until(
    () => host.calls.some((call) => call.command === "terminal_input" && call.args.key === "tmux_b" && String(call.args.data).includes("pasted")),
    "the clipboard's text typed into b",
  );
});

test("a tab strip landing mid-drag drops the seam drag, and keeps the new tab", async () => {
  measure();
  const before = JSON.stringify(stored().root.ratios);
  const seam = document.querySelector<HTMLElement>('.pane-divider[data-axis="row"]')!;
  pointer(seam, "pointerdown", 500, 300);
  pointer(window, "pointermove", 800, 300);
  host.tabs = [...host.tabs, tab("e")];
  strip();
  await until(() => tabEl("e") !== null, "e in a strip");
  pointer(window, "pointerup", 800, 300);
  await tick();
  assert.equal(JSON.stringify(stored().root.ratios), before, "the seam stayed where the new strip found it");
  assert.ok(panes().some((pane) => pane.includes("e")), "and e was not dropped by a stale layout");
});

test("the tab chords count tabs in the order the panes draw them", async () => {
  // a joins b's pane, after it: the panes read b, e, a; the host's order is
  // a, b, e.
  await fromMenu("a", '[data-move="' + group("g1").nextElementSibling?.getAttribute("data-group") + '"]');
  const drawn = [...document.querySelectorAll<HTMLElement>(".pane-strip .tab[data-key]")].map((one) => one.dataset.key!.slice(5));
  assert.equal(drawn[0], "b", `drawn ${drawn}`);
  window.dispatchEvent(new window.KeyboardEvent("keydown", { code: "Digit1", key: "1", ctrlKey: true, shiftKey: true, bubbles: true, cancelable: true }));
  await until(() => visible().includes("b"), "the first drawn tab, b, shown");
  assert.ok(panes().some((pane) => pane.endsWith("*b!")));
});
