// The window's rules over the tab-group layout: following the host's strip,
// and where a dragged tab lands. Plain functions, checked as a person would
// see the result: each group's tabs, the one it shows, and the focus.

import { test } from "node:test";
import assert from "node:assert/strict";
import { activateTab, focusGroup, groups, initialLayout, MAX_GROUPS, splitGroup, type Layout } from "./layout.ts";
import { dropAt, dropTab, dropZone, followHost } from "./panes.ts";

/** Each group as `id:tabs*active`, in reading order. */
function shape(layout: Layout): string[] {
  return groups(layout).map((group) => `${group.id}:${group.tabs.join(",")}*${group.active ?? ""}`);
}

/** g1:a,b,c | g2:d, focus on g1 showing b. */
function sideBySide(): Layout {
  const split = splitGroup(initialLayout(["a", "b", "c", "d"]), "g1", "right", "d");
  return activateTab(split, "b");
}

// ---- followHost ----

test("a group whose shown tab closed shows its most recently shown tab, not its neighbour", () => {
  // Shown in the order c, a, b: closing b goes back to a, as Orca does.
  const next = followHost(sideBySide(), ["a", "c", "d"], ["c", "d", "a", "b"]);
  assert.deepEqual(shape(next), ["g1:a,c*a", "g2:d*d"]);
  assert.equal(next.focused, "g1");
});

test("with no memory of the group, the closed tab's neighbour shows", () => {
  const next = followHost(sideBySide(), ["a", "c", "d"], []);
  assert.deepEqual(shape(next), ["g1:a,c*c", "g2:d*d"]);
});

test("a tab closing in an unfocused group leaves the focus where it is", () => {
  // g1:a,b,e showing b | g2:c, the keyboard in g2; b closes.
  const layout = focusGroup(activateTab(splitGroup(initialLayout(["a", "b", "c", "e"]), "g1", "right", "c"), "b"), "g2");
  const next = followHost(layout, ["a", "c", "e"], ["a", "e", "b"]);
  assert.deepEqual(shape(next), ["g1:a,e*e", "g2:c*c"]);
  assert.equal(next.focused, "g2", "the pane being looked at keeps the keyboard");
});

test("the last tab of a group closing collapses the group", () => {
  const next = followHost(sideBySide(), ["a", "b", "c"], ["d"]);
  assert.deepEqual(shape(next), ["g1:a,b,c*b"]);
  assert.equal(next.root.kind, "group");
});

test("following an unchanged strip returns the very same layout", () => {
  const layout = sideBySide();
  assert.equal(followHost(layout, ["d", "c", "b", "a"], ["a"]), layout);
  const once = followHost(layout, ["a", "d", "x"], ["a"]);
  assert.equal(followHost(once, ["a", "d", "x"], ["a"]), once);
});

// ---- dropZone, dropAt ----

const WINDOW = { left: 100, top: 50, width: 1000, height: 600 };

test("a drop on a body's middle joins it; an edge splits it that way", () => {
  const body = { left: 0, top: 0, width: 300, height: 300 };
  assert.equal(dropZone(body, { x: 150, y: 150 }), "center");
  assert.equal(dropZone(body, { x: 10, y: 150 }), "left");
  assert.equal(dropZone(body, { x: 295, y: 150 }), "right");
  assert.equal(dropZone(body, { x: 150, y: 10 }), "up");
  assert.equal(dropZone(body, { x: 150, y: 295 }), "down");
  assert.equal(dropZone(body, { x: 50, y: 5 }), "left", "a corner goes sideways, as Orca biases it");
});

test("dropAt finds the group under the pointer, its strip or its body", () => {
  const layout = sideBySide();
  // g1 is the left half: x 100..600; g2 the right: 600..1100. Strips 32px.
  assert.deepEqual(dropAt(layout, WINDOW, { x: 200, y: 60 }, 32, "c"), { kind: "strip", group: "g1", before: "c" });
  assert.deepEqual(dropAt(layout, WINDOW, { x: 200, y: 60 }, 32, "d"), { kind: "strip", group: "g1", before: null }, "a tab of another group is not a place in this strip");
  assert.deepEqual(dropAt(layout, WINDOW, { x: 900, y: 60 }, 32, null), { kind: "strip", group: "g2", before: null });
  assert.deepEqual(dropAt(layout, WINDOW, { x: 850, y: 350 }, 32, null), { kind: "body", group: "g2", zone: "center" });
  assert.deepEqual(dropAt(layout, WINDOW, { x: 1090, y: 350 }, 32, null), { kind: "body", group: "g2", zone: "right" });
  assert.deepEqual(dropAt(layout, WINDOW, { x: 350, y: 640 }, 32, null), { kind: "body", group: "g1", zone: "down" });
  assert.equal(dropAt(layout, WINDOW, { x: 50, y: 300 }, 32, null), null, "off the window");
});

// ---- dropTab ----

test("a tab dropped on another group's middle joins it, shown and focused", () => {
  const next = dropTab(sideBySide(), "c", { kind: "body", group: "g2", zone: "center" });
  assert.deepEqual(shape(next), ["g1:a,b*b", "g2:d,c*c"]);
  assert.equal(next.focused, "g2");
});

test("a tab dropped on another group's edge splits that group with it", () => {
  const next = dropTab(sideBySide(), "c", { kind: "body", group: "g2", zone: "down" });
  assert.deepEqual(shape(next), ["g1:a,b*b", "g2:d*d", "g3:c*c"]);
  assert.equal(next.focused, "g3");
  const left = dropTab(sideBySide(), "a", { kind: "body", group: "g2", zone: "left" });
  assert.deepEqual(shape(left), ["g1:b,c*b", "g3:a*a", "g2:d*d"]);
});

test("a group's only tab dropped on another group's edge moves there and its group collapses", () => {
  const next = dropTab(sideBySide(), "d", { kind: "body", group: "g1", zone: "up" });
  assert.deepEqual(shape(next), ["g3:d*d", "g1:a,b,c*b"]);
  assert.equal(next.root.kind === "split" && next.root.axis, "column");
});

test("a tab dropped on its own group's edge splits it, unless it is alone there", () => {
  const next = dropTab(sideBySide(), "a", { kind: "body", group: "g1", zone: "right" });
  assert.deepEqual(shape(next), ["g1:b,c*b", "g3:a*a", "g2:d*d"]);
  const layout = sideBySide();
  assert.equal(dropTab(layout, "d", { kind: "body", group: "g2", zone: "right" }), layout, "Orca refuses the same");
  assert.equal(dropTab(layout, "d", { kind: "body", group: "g2", zone: "center" }), layout);
});

test("a tab dropped on a strip lands before the tab it was dropped on", () => {
  assert.deepEqual(shape(dropTab(sideBySide(), "c", { kind: "strip", group: "g1", before: "a" })), ["g1:c,a,b*c", "g2:d*d"]);
  assert.deepEqual(shape(dropTab(sideBySide(), "a", { kind: "strip", group: "g1", before: null })), ["g1:b,c,a*a", "g2:d*d"]);
  assert.deepEqual(shape(dropTab(sideBySide(), "a", { kind: "strip", group: "g2", before: "d" })), ["g1:b,c*b", "g2:a,d*a"]);
  const layout = sideBySide();
  assert.equal(dropTab(layout, "b", { kind: "strip", group: "g1", before: "b" }), layout, "on itself");
  assert.equal(dropTab(layout, "b", { kind: "strip", group: "g9", before: null }), layout, "an unknown group");
});

test("a split the layout has no room for leaves the dragged tab where it was", () => {
  const keys = Array.from({ length: MAX_GROUPS + 1 }, (_, index) => `k${index}`);
  let layout = initialLayout(keys);
  for (const key of keys.slice(1, MAX_GROUPS)) layout = splitGroup(layout, "g1", "right", key);
  assert.equal(groups(layout).length, MAX_GROUPS);
  const last = groups(layout)[groups(layout).length - 1].id;
  assert.equal(dropTab(layout, "k0", { kind: "body", group: last, zone: "down" }), layout);
});
