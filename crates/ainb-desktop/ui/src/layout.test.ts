// `layout.ts` holds no store and no DOM, so it is tested here as plain
// functions: build a layout by the same steps a person would take, then check
// what they would see. Every test also checks the module's invariants, and a
// seeded random walk checks them after each of a few thousand steps.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  activateTab,
  addTab,
  focusGroup,
  geometry,
  groupOf,
  groups,
  initialLayout,
  LAYOUT_KEY,
  MAX_GROUPS,
  MIN_SHARE,
  moveTab,
  parse,
  readLayout,
  reconcile,
  removeTab,
  resize,
  serialize,
  splitGroup,
  writeLayout,
  type Layout,
  type LayoutNode,
  type LayoutStorage,
} from "./layout.ts";

/** Assert every invariant `layout.ts` promises; with `live`, that the layout
 * holds exactly those keys. */
function assertInvariants(layout: Layout, live?: readonly string[]): void {
  const all = groups(layout);
  const ids = all.map((group) => group.id);
  assert.equal(new Set(ids).size, ids.length, "group ids are unique");
  assert.ok(ids.length <= MAX_GROUPS, `at most ${MAX_GROUPS} groups`);
  for (const id of ids) assert.ok(Number(id.slice(1)) < layout.next, `${id} was minted before next=${layout.next}`);
  assert.ok(ids.includes(layout.focused), "the focus names a group");

  const keys = all.flatMap((group) => group.tabs);
  assert.equal(new Set(keys).size, keys.length, "every tab sits in exactly one group");
  if (live !== undefined) assert.deepEqual([...keys].sort(), [...new Set(live)].sort(), "the layout holds the live tabs");

  for (const group of all) {
    if (group.tabs.length === 0) {
      assert.equal(all.length, 1, `${group.id} is empty beside other groups`);
      assert.equal(group.active, null, `${group.id} is empty, so it shows nothing`);
    } else {
      assert.ok(group.active !== null && group.tabs.includes(group.active), `${group.id} shows one of its own tabs`);
    }
  }

  const walk = (node: LayoutNode): void => {
    if (node.kind === "group") return;
    assert.ok(node.children.length >= 2, "a split has two or more children");
    assert.equal(node.ratios.length, node.children.length, "one ratio per child");
    assert.ok(node.ratios.every((ratio) => ratio > 0), "ratios are positive");
    const sum = node.ratios.reduce((total, ratio) => total + ratio, 0);
    assert.ok(Math.abs(sum - 1) < 1e-9, `ratios sum to 1, not ${sum}`);
    const floor = Math.min(MIN_SHARE, 0.5 / node.ratios.length);
    assert.ok(node.ratios.every((ratio) => ratio >= floor - 1e-12), `no pane under ${floor} of its split: ${node.ratios}`);
    for (const child of node.children) {
      if (child.kind === "split") assert.notEqual(child.axis, node.axis, "a split never nests one along its own axis");
      walk(child);
    }
  };
  walk(layout.root);
}

/** Each group as `id:tabs*active`, in reading order: compact enough to
 * assert a whole layout's contents in one line. */
function shape(layout: Layout): string[] {
  return groups(layout).map((group) => `${group.id}:${group.tabs.join(",")}*${group.active ?? ""}`);
}

/** Assert `actual` is `expected`, number by number, to within rounding. */
function assertNear(actual: readonly number[] | null, expected: readonly number[], message?: string): void {
  assert.ok(
    actual !== null && actual.length === expected.length && actual.every((value, at) => Math.abs(value - expected[at]) < 1e-12),
    `${message ?? "ratios"}: ${JSON.stringify(actual)} is not ${JSON.stringify(expected)}`,
  );
}

/** The root split's ratios, or `null` for a lone group. */
function rootRatios(layout: Layout): readonly number[] | null {
  return layout.root.kind === "split" ? layout.root.ratios : null;
}

/** g1:a,c | g2:b, with the focus on g2. */
function twoColumns(): Layout {
  return splitGroup(initialLayout(["a", "b", "c"]), "g1", "right", "b");
}

/** g1:a | (g2:b over g3:c), with the focus on g3. */
function leftAndStack(): Layout {
  return splitGroup(moveTab(twoColumns(), "c", "g2"), "g2", "down");
}

/** A fake storage over a map. */
function memoryStorage(initial: Record<string, string> = {}): LayoutStorage & { data: Map<string, string> } {
  const data = new Map(Object.entries(initial));
  return {
    data,
    getItem: (key) => data.get(key) ?? null,
    setItem: (key, value) => void data.set(key, value),
  };
}
const throwingStorage: LayoutStorage = {
  getItem: () => {
    throw new Error("blocked");
  },
  setItem: () => {
    throw new Error("blocked");
  },
};

/** mulberry32: a seeded generator, so a failure replays exactly. */
function seeded(seed: number): () => number {
  return () => {
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

// ---- initialLayout, groupOf ----

test("the first layout is one group holding every tab, the first one shown", () => {
  const layout = initialLayout(["a", "b", "a", "c"]);
  assert.deepEqual(shape(layout), ["g1:a,b,c*a"], "a repeated key is placed once");
  assert.equal(layout.focused, "g1");
  assert.equal(groupOf(layout, "b"), "g1");
  assert.equal(groupOf(layout, "zzz"), null);
  assertInvariants(layout, ["a", "b", "c"]);
  assertInvariants(initialLayout([]), []);
});

// ---- splitGroup ----

test("splitting right moves the named tab into a new focused group beside it", () => {
  const layout = twoColumns();
  assert.deepEqual(shape(layout), ["g1:a,c*a", "g2:b*b"]);
  assert.equal(layout.focused, "g2");
  assert.equal(layout.root.kind === "split" && layout.root.axis, "row");
  assert.deepEqual(rootRatios(layout), [0.5, 0.5]);
  assertInvariants(layout, ["a", "b", "c"]);
});

test("without a named tab, the group's shown tab is the one that moves", () => {
  const layout = splitGroup(activateTab(initialLayout(["a", "b", "c"]), "b"), "g1", "down");
  assert.deepEqual(shape(layout), ["g1:a,c*c", "g2:b*b"]);
  assert.equal(layout.root.kind === "split" && layout.root.axis, "column");
  assertInvariants(layout, ["a", "b", "c"]);
});

test("the old group shows the neighbour of a moved active tab", () => {
  const start = activateTab(initialLayout(["a", "b", "c"]), "b");
  assert.deepEqual(shape(splitGroup(start, "g1", "down", "b")), ["g1:a,c*c", "g2:b*b"], "the tab after it");
  const last = activateTab(initialLayout(["a", "b", "c"]), "c");
  assert.deepEqual(shape(splitGroup(last, "g1", "down", "c")), ["g1:a,b*b", "g2:c*c"], "else the one before");
});

test("a split along the parent's own axis joins it rather than nesting", () => {
  // g1 | g2, then g2 split right again: three columns, not a row in a row.
  const layout = splitGroup(moveTab(splitGroup(initialLayout(["a", "b", "c", "d"]), "g1", "right", "b"), "c", "g2"), "g2", "right");
  assert.deepEqual(shape(layout), ["g1:a,d*a", "g2:b*b", "g3:c*c"]);
  assert.equal(layout.root.kind === "split" && layout.root.axis, "row");
  assert.deepEqual(rootRatios(layout), [0.5, 0.25, 0.25], "the split group gives the new one half of its share");
  assertInvariants(layout, ["a", "b", "c", "d"]);
});

test("a split that would leave a group empty is not made, nor one with bad arguments", () => {
  const lone = initialLayout(["a"]);
  assert.equal(splitGroup(lone, "g1", "right"), lone, "a group's only tab stays");
  assert.equal(splitGroup(lone, "g1", "right", "a"), lone, "even when named");
  const empty = initialLayout([]);
  assert.equal(splitGroup(empty, "g1", "down"), empty, "an empty group");
  const layout = initialLayout(["a", "b"]);
  assert.equal(splitGroup(layout, "g9", "right"), layout, "an unknown group");
  assert.equal(splitGroup(layout, "g1", "right", "zzz"), layout, "a tab the group does not hold");
});

test(`a layout stops splitting at ${MAX_GROUPS} groups, and that layout still round trips`, () => {
  const keys = Array.from({ length: MAX_GROUPS + 1 }, (_, index) => `k${index}`);
  let layout = initialLayout(keys);
  for (const key of keys.slice(1, MAX_GROUPS)) layout = splitGroup(layout, "g1", "right", key);
  assert.equal(groups(layout).length, MAX_GROUPS);
  assert.equal(splitGroup(layout, "g1", "right", keys[MAX_GROUPS]), layout);
  assertInvariants(layout, keys);
  assert.deepEqual(parse(serialize(layout)), layout);
});

test("splitting left or up puts the new group before the old one", () => {
  const left = splitGroup(initialLayout(["a", "b"]), "g1", "left", "b");
  assert.deepEqual(shape(left), ["g2:b*b", "g1:a*a"]);
  assert.equal(left.root.kind === "split" && left.root.axis, "row");
  assert.equal(left.focused, "g2");
  const up = splitGroup(initialLayout(["a", "b"]), "g1", "up", "b");
  assert.deepEqual(shape(up), ["g2:b*b", "g1:a*a"]);
  assert.equal(up.root.kind === "split" && up.root.axis, "column");
  assertInvariants(left, ["a", "b"]);
  assertInvariants(up, ["a", "b"]);
});

test("a layout at the end of the id space stops splitting, and still loads", () => {
  // The last id there is room for is g999999999; a stored layout may have
  // reached it. Splitting further would mint a tenth digit, which `parse`
  // refuses, and the whole layout would be lost on the next launch.
  const at = (next: number) =>
    parse(JSON.stringify({ version: 1, focused: "g1", next, root: { kind: "group", id: "g1", tabs: ["a", "b", "c"], active: "a" } }));
  const last = splitGroup(at(999_999_999), "g1", "right", "b");
  assert.deepEqual(shape(last), ["g1:a,c*a", "g999999999:b*b"], "the last id is still minted");
  assert.deepEqual(parse(serialize(last)), last);
  const full = parse(serialize(last));
  assert.equal(splitGroup(full, "g1", "right", "c"), full, "no id is left to mint");
  assert.deepEqual(parse(serialize(splitGroup(full, "g1", "right", "c"))), full);
});

// ---- a group closing ----

test("a group its last tab left gives its space and the focus to the sibling before it", () => {
  // g1 split twice: g1 | g3 | g2, the newest beside the group it came from.
  const three = splitGroup(twoColumns(), "g1", "right", "c");
  assert.deepEqual(shape(three), ["g1:a*a", "g3:c*c", "g2:b*b"]);
  assert.deepEqual(rootRatios(three), [0.25, 0.25, 0.5]);
  const closed = removeTab(three, "c");
  assert.deepEqual(shape(closed), ["g1:a*a", "g2:b*b"]);
  assert.equal(closed.focused, "g1", "the focus goes where the space went");
  assert.deepEqual(rootRatios(closed), [0.5, 0.5]);
  assertInvariants(closed, ["a", "b"]);
});

test("the first group closes into the one after it, which takes the focus", () => {
  const closed = reconcile(focusGroup(twoColumns(), "g1"), ["b"]);
  assert.deepEqual(shape(closed), ["g2:b*b"]);
  assert.equal(closed.focused, "g2");
  assert.equal(closed.root.kind, "group", "a split of one child is that child");
  assertInvariants(closed, ["b"]);
});

test("a group closes into the nearest group of a split sibling, which takes its space", () => {
  const layout = leftAndStack();
  assert.deepEqual(shape(layout), ["g1:a*a", "g2:b*b", "g3:c*c"]);
  const closed = removeTab(layout, "a");
  assert.deepEqual(shape(closed), ["g2:b*b", "g3:c*c"]);
  assert.equal(closed.root.kind === "split" && closed.root.axis, "column", "the stack fills the window");
  assert.equal(closed.focused, "g3", "an unfocused group's close leaves the focus");
  assertInvariants(closed, ["b", "c"]);
});

// ---- moveTab ----

test("moving a tab shows it in its new group and focuses that group", () => {
  const layout = focusGroup(splitGroup(initialLayout(["a", "b", "c"]), "g1", "right", "c"), "g1");
  const moved = moveTab(layout, "b", "g2");
  assert.deepEqual(shape(moved), ["g1:a*a", "g2:c,b*b"]);
  assert.equal(moved.focused, "g2");
  assertInvariants(moved, ["a", "b", "c"]);
});

test("moving a group's last tab out closes it", () => {
  const moved = moveTab(twoColumns(), "b", "g1");
  assert.deepEqual(shape(moved), ["g1:a,c,b*b"]);
  assert.equal(moved.focused, "g1");
  assert.equal(moved.root.kind, "group");
  assertInvariants(moved, ["a", "b", "c"]);
});

test("a move to an unknown group, of an unknown tab, or to its own group changes nothing", () => {
  const layout = initialLayout(["a", "b"]);
  assert.equal(moveTab(layout, "a", "g9"), layout);
  assert.equal(moveTab(layout, "zzz", "g1"), layout);
  assert.equal(moveTab(layout, "a", "g1"), layout);
});

test("a tab moved to an index lands there, within its group or another", () => {
  const within = moveTab(initialLayout(["a", "b", "c"]), "a", "g1", 2);
  assert.deepEqual(shape(within), ["g1:b,c,a*a"], "reordered, and shown");
  assert.deepEqual(shape(moveTab(initialLayout(["a", "b", "c"]), "c", "g1", 0)), ["g1:c,a,b*c"]);
  const across = moveTab(twoColumns(), "c", "g2", 0);
  assert.deepEqual(shape(across), ["g1:a*a", "g2:c,b*c"]);
  assert.equal(across.focused, "g2");
  assert.deepEqual(shape(moveTab(twoColumns(), "c", "g2", 99)), ["g1:a*a", "g2:b,c*c"], "past the end is the end");
  assert.deepEqual(shape(moveTab(twoColumns(), "c", "g2", -3)), ["g1:a*a", "g2:c,b*c"], "before the start is the start");
  assert.deepEqual(shape(moveTab(twoColumns(), "c", "g2", Number.NaN)), ["g1:a*a", "g2:b,c*c"], "no number is the end");
  const still = focusGroup(activateTab(initialLayout(["a", "b"]), "b"), "g1");
  assert.equal(moveTab(still, "b", "g1", 1), still, "a tab dropped where it is changes nothing");
  assertInvariants(within, ["a", "b", "c"]);
  assertInvariants(across, ["a", "b", "c"]);
});

// ---- addTab ----

test("a tab is added to the focused group without taking what it shows", () => {
  const layout = twoColumns();
  const added = addTab(layout, "d");
  assert.deepEqual(shape(added), ["g1:a,c*a", "g2:b,d*b"]);
  assert.equal(added.focused, "g2");
  const elsewhere = addTab(layout, "d", "g1");
  assert.deepEqual(shape(elsewhere), ["g1:a,c,d*a", "g2:b*b"], "or to a named group");
  assert.equal(elsewhere.focused, "g2", "a tab opened in the background leaves the focus where it was");
  assert.deepEqual(shape(addTab(initialLayout([]), "x")), ["g1:x*x"], "an empty group shows its first tab");
  assertInvariants(added, ["a", "b", "c", "d"]);
});

test("adding a tab already placed moves it, focus and all, so it is never in two groups", () => {
  const layout = focusGroup(twoColumns(), "g1");
  const again = addTab(layout, "a", "g2");
  assert.deepEqual(shape(again), ["g1:c*c", "g2:b,a*a"]);
  assert.equal(again.focused, "g2");
  assertInvariants(again, ["a", "b", "c"]);
  assert.equal(addTab(layout, "d", "g9"), layout, "an unknown group changes nothing");
});

// ---- removeTab ----

test("removing a tab shows its neighbour, and an emptied group closes", () => {
  const layout = twoColumns();
  assert.deepEqual(shape(removeTab(layout, "a")), ["g1:c*c", "g2:b*b"]);
  const emptied = removeTab(layout, "b");
  assert.deepEqual(shape(emptied), ["g1:a,c*a"]);
  assert.equal(emptied.focused, "g1", "the focus goes where the space went");
  assertInvariants(emptied, ["a", "c"]);
});

test("the last group stays when its last tab goes, empty for the next", () => {
  const empty = removeTab(initialLayout(["a"]), "a");
  assert.deepEqual(shape(empty), ["g1:*"]);
  assertInvariants(empty, []);
  const layout = initialLayout(["a"]);
  assert.equal(removeTab(layout, "zzz"), layout, "an unknown tab changes nothing");
});

// ---- activateTab, focusGroup ----

test("activating a tab shows it and focuses its group", () => {
  const shown = activateTab(twoColumns(), "c");
  assert.deepEqual(shape(shown), ["g1:a,c*c", "g2:b*b"]);
  assert.equal(shown.focused, "g1");
  const layout = twoColumns();
  assert.equal(activateTab(layout, "zzz"), layout);
});

test("focusing a group moves nothing else; an unknown group or the focused one changes nothing", () => {
  const layout = leftAndStack();
  const moved = focusGroup(layout, "g1");
  assert.equal(moved.focused, "g1");
  assert.deepEqual(shape(moved), shape(layout));
  assert.equal(focusGroup(layout, "g9"), layout);
  assert.equal(focusGroup(layout, "g3"), layout);
});

// ---- resize, geometry ----

test("resize moves the seam between two neighbours, never past the minimum share", () => {
  const layout = twoColumns();
  assertNear(rootRatios(resize(layout, [], 0, 0.7)), [0.7, 0.3]);
  assertNear(rootRatios(resize(layout, [], 0, 0.99)), [1 - MIN_SHARE, MIN_SHARE], "clamped on the right");
  assertNear(rootRatios(resize(layout, [], 0, -5)), [MIN_SHARE, 1 - MIN_SHARE], "clamped on the left");
  for (const [path, index, share] of [
    [[], 0, Number.NaN],
    [[], 1, 0.5],
    [[], -1, 0.5],
    [[3], 0, 0.5],
    [[0], 0, 0.5],
  ] as const) {
    assert.equal(resize(layout, path, index, share), layout, `path ${path} index ${index} share ${share}`);
  }
  const lone = initialLayout(["a"]);
  assert.equal(resize(lone, [], 0, 0.3), lone, "a lone group has no seam");
  for (const share of [0.3, 1e-9, 1, 0.5]) assertInvariants(resize(layout, [], 0, share), ["a", "b", "c"]);
});

test("resize reaches a nested split, and moves only the two panes beside the seam", () => {
  const nested = resize(leftAndStack(), [1], 0, 0.25);
  const column = nested.root.kind === "split" ? nested.root.children[1] : null;
  assertNear(column?.kind === "split" ? column.ratios : null, [0.25, 0.75]);
  assert.deepEqual(rootRatios(nested), [0.5, 0.5], "the outer split is untouched");

  const three = splitGroup(twoColumns(), "g1", "right", "c");
  assert.deepEqual(rootRatios(three), [0.25, 0.25, 0.5]);
  const squeezed = resize(three, [], 0, 0);
  assertNear(rootRatios(squeezed), [MIN_SHARE, 0.5 - MIN_SHARE, 0.5], "the third pane keeps its share");
  assertInvariants(squeezed, ["a", "b", "c"]);
});

test("a seam still moves in a split of more panes than the minimum leaves room for", () => {
  const count = 8;
  const raw = JSON.stringify({
    version: 1,
    focused: "g1",
    next: count + 1,
    root: {
      kind: "split",
      axis: "row",
      ratios: Array.from({ length: count }, () => 1),
      children: Array.from({ length: count }, (_, at) => ({ kind: "group", id: `g${at + 1}`, tabs: [`k${at}`], active: `k${at}` })),
    },
  });
  const layout = parse(raw);
  const wider = resize(layout, [], 0, 0.9);
  assert.notEqual(wider, layout, "the seam moved");
  assert.ok(rootRatios(wider)![0] > 1 / count, `the first pane grew: ${rootRatios(wider)}`);
  assertInvariants(wider);
});

test("a seam dragged against its stop, or a tab already shown, returns the layout as it was", () => {
  const stopped = resize(twoColumns(), [], 0, 0.99);
  assert.equal(resize(stopped, [], 0, 2), stopped, "nothing left to move");
  const layout = twoColumns();
  assert.equal(activateTab(layout, "b"), layout, "b is already shown in the focused group");
});

test("a stored sliver of a pane loads at the minimum share", () => {
  const raw = serialize(twoColumns()).replace('"ratios":[0.5,0.5]', '"ratios":[1,5e-324]');
  assert.ok(raw.includes("5e-324"), "the fixture edits the stored ratios");
  const layout = parse(raw);
  assert.deepEqual(shape(layout), ["g1:a,c*a", "g2:b*b"]);
  assertNear(rootRatios(layout), [1 - MIN_SHARE, MIN_SHARE]);
  assertInvariants(layout, ["a", "b", "c"]);
  assert.deepEqual(parse(serialize(layout)), layout);
});

test("geometry places each group and each seam as a share of the window", () => {
  const { groups: rects, dividers } = geometry(leftAndStack());
  assert.deepEqual(Object.fromEntries(rects), {
    g1: { x: 0, y: 0, w: 0.5, h: 1 },
    g2: { x: 0.5, y: 0, w: 0.5, h: 0.5 },
    g3: { x: 0.5, y: 0.5, w: 0.5, h: 0.5 },
  });
  assert.deepEqual(dividers, [
    { path: [], index: 0, axis: "row", pair: { x: 0, y: 0, w: 1, h: 1 }, at: 0.5 },
    { path: [1], index: 0, axis: "column", pair: { x: 0.5, y: 0, w: 0.5, h: 1 }, at: 0.5 },
  ]);
  assert.deepEqual(Object.fromEntries(geometry(initialLayout(["a"])).groups), { g1: { x: 0, y: 0, w: 1, h: 1 } });
  assert.deepEqual(geometry(initialLayout(["a"])).dividers, []);
});

// ---- reconcile ----

test("reconcile drops tabs the host no longer lists and adds new ones to the focus", () => {
  const next = reconcile(twoColumns(), ["a", "b", "d", "e"]);
  assert.deepEqual(shape(next), ["g1:a*a", "g2:b,d,e*b"]);
  assertInvariants(next, ["a", "b", "d", "e"]);
});

test("reconcile closes a group whose tabs all ended", () => {
  const next = reconcile(twoColumns(), ["a", "c"]);
  assert.deepEqual(shape(next), ["g1:a,c*a"]);
  assert.equal(next.focused, "g1");
  assertInvariants(next, ["a", "c"]);
  assert.deepEqual(shape(reconcile(next, [])), ["g1:*"], "every tab gone leaves one empty group");
});

test("reconcile is idempotent", () => {
  const layout = leftAndStack();
  for (const live of [["a", "b", "c"], ["c", "x"], [], ["y", "y", "a"]]) {
    const once = reconcile(layout, live);
    assert.deepEqual(reconcile(once, live), once, `twice with ${JSON.stringify(live)}`);
    assertInvariants(once, live);
  }
  assert.equal(reconcile(layout, ["c", "b", "a"]), layout, "nothing to do returns the layout as it was");
});

// ---- serialize, parse ----

test("a layout survives a serialize and parse round trip", () => {
  const layout = activateTab(addTab(leftAndStack(), "d", "g1"), "d");
  const back = parse(serialize(layout));
  assert.deepEqual(back, layout);
  assertInvariants(back, ["a", "b", "c", "d"]);
  // Ids keep counting from where they were, not from the highest left.
  const closed = removeTab(layout, "c");
  assert.equal(groupOf(closed, "c"), null, "g3, c's group, closed");
  assert.equal(parse(serialize(closed)).next, layout.next);
  // The one empty group a layout can have round trips too, id and all.
  const emptied = reconcile(layout, []);
  assert.deepEqual(parse(serialize(emptied)), emptied);
  assert.notEqual(groups(emptied)[0].id, "g1");
});

test("parse turns anything it could not have written into one empty group", () => {
  const good = JSON.parse(serialize(twoColumns()));
  const group = (id: string, tabs: unknown[], active: unknown) => ({ kind: "group", id, tabs, active });
  const split = (children: unknown[], ratios: unknown[]) => ({ kind: "split", axis: "row", children, ratios });
  const pair = split([group("g1", ["a"], "a"), group("g2", ["b"], "b")], [0.5, 0.5]);
  const one = (root: unknown, extra: Record<string, unknown> = {}) =>
    JSON.stringify({ ...good, root, focused: "g1", ...extra });
  const many = split(
    Array.from({ length: MAX_GROUPS + 1 }, (_, index) => group(`g${index + 1}`, [`k${index}`], `k${index}`)),
    Array.from({ length: MAX_GROUPS + 1 }, () => 1),
  );
  // Deep enough to overflow a recursive walk; built as text, since
  // `JSON.stringify` would overflow building it.
  const depth = 200_000;
  const deep = `{"version":1,"focused":"g1","next":2,"root":${'{"kind":"split","axis":"row","ratios":[1],"children":['.repeat(depth)}${JSON.stringify(group("g1", ["a"], "a"))}${"]}".repeat(depth)}}`;
  const garbage: Record<string, string> = {
    empty: "",
    "not json": "not json",
    null: "null",
    array: "[]",
    number: "42",
    "another version": JSON.stringify({ ...good, version: 2 }),
    "focus on no group": JSON.stringify({ ...good, focused: "g9" }),
    "a tab twice in a group": one(group("g1", ["a", "a"], "a")),
    "a tab in two groups": one(split([group("g1", ["a"], "a"), group("g2", ["a"], "a")], [0.5, 0.5])),
    "a group id twice": one(split([group("g1", ["a"], "a"), group("g1", ["b"], "b")], [0.5, 0.5])),
    "active outside its group": one(group("g1", ["a"], "b")),
    "an empty lone group showing a tab": one(group("g1", [], "a")),
    "an empty group beside another": one(split([group("g1", ["a"], "a"), group("g2", [], null)], [0.5, 0.5])),
    "a malformed id": JSON.stringify({ ...good, root: group("x1", ["a"], "a"), focused: "x1" }),
    "an id past nine digits": JSON.stringify({ ...good, root: group("g9007199254740992", ["a"], "a"), focused: "g9007199254740992" }),
    "a tab that is not a string": one(group("g1", [1], 1)),
    "a negative ratio": one(pair, { root: { ...pair, ratios: [1, -1] } }),
    "ratios that sum to Infinity": one(pair, { root: { ...pair, ratios: [1e308, 1e308] } }),
    "a ratio per child missing": one(pair, { root: { ...pair, ratios: [1] } }),
    "a split of nothing": one(split([], [])),
    "an unknown axis": one({ ...pair, axis: "diagonal" }),
    "an unknown kind": one({ kind: "tab" }),
    "next of zero": one(group("g1", ["a"], "a"), { next: 0 }),
    "next past the id space": one(group("g1", ["a"], "a"), { next: 1e12 }),
    "next not an integer": one(group("g1", ["a"], "a"), { next: 2.5 }),
    [`${MAX_GROUPS + 1} groups`]: one(many),
    "a tree too deep to walk": deep,
  };
  for (const [name, raw] of Object.entries(garbage)) {
    assert.deepEqual(parse(raw), initialLayout([]), name);
  }
});

test("parse restores a hand-edited but valid layout to its canonical shape", () => {
  const raw = JSON.stringify({
    version: 1,
    focused: "g2",
    next: 1,
    root: {
      kind: "split",
      axis: "row",
      ratios: [2, 2],
      children: [
        { kind: "group", id: "g5", tabs: ["a"], active: "a" },
        {
          kind: "split",
          axis: "row",
          ratios: [1, 3],
          children: [
            { kind: "group", id: "g2", tabs: ["b"], active: "b" },
            { kind: "group", id: "g3", tabs: ["c"], active: "c" },
          ],
        },
      ],
    },
  });
  const layout = parse(raw);
  assert.deepEqual(shape(layout), ["g5:a*a", "g2:b*b", "g3:c*c"]);
  // Spliced and scaled, [0.5, 0.125, 0.375], then the sliver raised to the
  // minimum share, paid for by the others in proportion to what they hold
  // over it.
  const ratios = rootRatios(layout)!;
  assert.equal(ratios[1], MIN_SHARE, "a row in a row is spliced, and no pane is thinner than the minimum");
  assert.ok(Math.abs((ratios[0] - MIN_SHARE) / (ratios[2] - MIN_SHARE) - (0.5 - MIN_SHARE) / (0.375 - MIN_SHARE)) < 1e-9);
  assert.equal(layout.next, 6, "never mints an id the tree holds");
  assertInvariants(layout, ["a", "b", "c"]);
});

test("parse keeps every invariant over a thousand corruptions of a real layout", () => {
  const random = seeded(0xbad);
  const base = serialize(activateTab(addTab(leftAndStack(), "d", "g1"), "d"));
  const junk: unknown[] = [null, 0, -1, 1e308, 2 ** 53, "", "g1", "g2", "g9", "a", "b", [], {}, ["a"], true, NaN];
  /** Every path in `value` to a replaceable slot. */
  const paths = (value: unknown, at: (string | number)[] = []): (string | number)[][] => {
    if (typeof value !== "object" || value === null) return [at];
    const entries = Array.isArray(value) ? value.map((child, index) => [index, child] as const) : Object.entries(value);
    return [at, ...entries.flatMap(([key, child]) => paths(child, [...at, key]))];
  };
  for (let round = 0; round < 1000; round += 1) {
    let value: unknown = JSON.parse(base);
    for (let edit = 0; edit < 1 + Math.floor(random() * 3); edit += 1) {
      const all = paths(value);
      const path = all[Math.floor(random() * all.length)];
      const replacement = junk[Math.floor(random() * junk.length)];
      if (path.length === 0) value = replacement;
      else {
        let parent = value as Record<string | number, unknown>;
        for (const key of path.slice(0, -1)) parent = parent[key] as Record<string | number, unknown>;
        parent[path[path.length - 1]] = replacement;
      }
    }
    const raw = JSON.stringify(value) ?? "";
    const layout = parse(raw);
    try {
      assertInvariants(layout);
      assert.deepEqual(parse(serialize(layout)), layout, "what parse accepts round trips");
    } catch (error) {
      console.error({ round, raw });
      throw error;
    }
  }
});

// ---- readLayout, writeLayout ----

test("a stored layout comes back following the host's tabs", () => {
  const storage = memoryStorage();
  const layout = twoColumns();
  writeLayout(storage, layout);
  assert.ok(storage.data.has(LAYOUT_KEY));
  assert.deepEqual(readLayout(storage, ["a", "b", "c"]), layout);
  // "c" ended and "d" opened while the window was away.
  assert.deepEqual(shape(readLayout(storage, ["a", "b", "d"])), ["g1:a*a", "g2:b,d*b"]);
});

test("missing, throwing or corrupt storage gives one group holding every live tab", () => {
  const live = ["a", "b", "c"];
  for (const storage of [undefined, memoryStorage(), throwingStorage, memoryStorage({ [LAYOUT_KEY]: "{oops" })]) {
    const layout = readLayout(storage, live);
    assert.deepEqual(shape(layout), ["g1:a,b,c*a"]);
    assertInvariants(layout, live);
  }
  assert.doesNotThrow(() => writeLayout(throwingStorage, initialLayout(live)));
  assert.doesNotThrow(() => writeLayout(undefined, initialLayout(live)));
});

// ---- flows ----

test("flow: split right, move a tab, the left group's tabs end, every tab left still reachable", () => {
  const live = ["a", "b", "c", "d"];
  let layout = readLayout(undefined, live);
  layout = splitGroup(layout, "g1", "right", "b");
  layout = moveTab(layout, "c", "g2");
  assert.deepEqual(shape(layout), ["g1:a,d*a", "g2:b,c*c"]);
  assert.equal(layout.focused, "g2");

  layout = reconcile(layout, ["b", "c", "e"]);
  assert.deepEqual(shape(layout), ["g2:b,c,e*c"], "the right group takes the window, and the new tab");
  assert.equal(layout.focused, "g2");
  for (const key of ["b", "c", "e"]) {
    const shown = activateTab(layout, key);
    assert.equal(shown.focused, groupOf(shown, key), `${key} is reachable`);
  }
  assertInvariants(layout, ["b", "c", "e"]);
});

test("flow: open a second session and split it out beside the first, then it ends", () => {
  // The host opens "two" and focuses it; the window splits it out.
  let layout = readLayout(undefined, ["one"]);
  layout = activateTab(reconcile(layout, ["one", "two"]), "two");
  layout = splitGroup(layout, layout.focused, "right");
  assert.deepEqual(shape(layout), ["g1:one*one", "g2:two*two"]);
  assert.equal(layout.focused, "g2");

  // The second session ends: its pane closes and the focus goes back left.
  layout = reconcile(layout, ["one"]);
  assert.deepEqual(shape(layout), ["g1:one*one"]);
  assert.equal(layout.focused, "g1");
  assertInvariants(layout, ["one"]);
});

test("flow: a grid survives a reload, then its tabs ending one by one", () => {
  const live = ["a", "b", "c", "d"];
  let layout = readLayout(undefined, live);
  layout = splitGroup(layout, "g1", "right", "b");
  layout = splitGroup(layout, "g1", "down", "c");
  layout = moveTab(layout, "d", "g2");
  layout = splitGroup(layout, "g2", "down");
  // (g1 over g3) | (g2 over g4)
  assert.deepEqual(shape(layout), ["g1:a*a", "g3:c*c", "g2:b*b", "g4:d*d"]);

  const storage = memoryStorage();
  writeLayout(storage, layout);
  let reloaded = readLayout(storage, live);
  assert.deepEqual(reloaded, layout);

  for (const [index, gone] of live.entries()) {
    const remaining = live.slice(index + 1);
    reloaded = reconcile(reloaded, remaining);
    assertInvariants(reloaded, remaining);
    assert.equal(groupOf(reloaded, gone), null);
    for (const key of remaining) assert.notEqual(groupOf(reloaded, key), null, `${key} survives ${gone} ending`);
  }
  assert.deepEqual(groups(reloaded).map((group) => group.tabs), [[]], "the last tab gone leaves one empty group");
});

test("flow: a long random walk of every step keeps every invariant", () => {
  const random = seeded(0x5eed);
  const pick = <T,>(items: readonly T[]): T => items[Math.floor(random() * items.length)];
  const pool = ["a", "b", "c", "d", "e", "f", "g", "h"];
  let live = ["a", "b"];
  let layout = readLayout(undefined, live);

  for (let step = 0; step < 3000; step += 1) {
    const ids = groups(layout).map((group) => group.id);
    const before = layout;
    switch (Math.floor(random() * 11)) {
      case 0:
        layout = splitGroup(layout, pick(ids), pick(["right", "down", "left", "up"] as const), random() < 0.5 ? pick(pool) : undefined);
        break;
      case 2:
        layout = moveTab(layout, pick(pool), pick(ids), random() < 0.5 ? Math.floor(random() * 5) - 1 : undefined);
        break;
      case 3:
        layout = activateTab(layout, pick(pool));
        break;
      case 4:
        layout = focusGroup(layout, pick(ids));
        break;
      case 9: {
        // A seam dragged anywhere, even off the window.
        const seams = geometry(layout).dividers;
        if (seams.length > 0) {
          const seam = pick(seams);
          layout = resize(layout, seam.path, seam.index, random() * 1.4 - 0.2);
        }
        break;
      }
      case 5: {
        // The host opens one tab, told to the layout directly.
        const key = pick(pool);
        if (!live.includes(key)) live = [...live, key];
        layout = addTab(layout, key, random() < 0.5 ? pick(ids) : undefined);
        break;
      }
      case 6: {
        // The host closes one tab, told to the layout directly.
        const key = pick(pool);
        live = live.filter((other) => other !== key);
        layout = removeTab(layout, key);
        break;
      }
      default:
        // The host's whole tab list, opened and closed at once.
        live = pool.filter((key) => (live.includes(key) ? random() < 0.85 : random() < 0.15));
        layout = reconcile(layout, live);
        assert.deepEqual(reconcile(layout, live), layout, `reconcile is idempotent at step ${step}`);
    }
    try {
      assertInvariants(layout, live);
      assert.deepEqual(parse(serialize(layout)), layout, "round trip");
    } catch (error) {
      console.error({ step, before: serialize(before), after: serialize(layout) });
      throw error;
    }
  }
});
