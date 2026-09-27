// Orca-style tab groups in a split tree: which terminal tabs sit side by side
// or stacked, and which group has the keyboard. A pure model, in the manner of
// `sidebar_model.ts`: no store, no DOM, every function returns a new layout.
//
//   ┌──────────────┬──────────────┐
//   │ g1  [a] b    │ g2  [c]      │   row: g1 | column(g2 over g3)
//   │              ├──────────────┤   ratios 0.5 / 0.5, then 0.5 / 0.5
//   │              │ g3  [d] e    │   [x] is each group's active tab
//   └──────────────┴──────────────┘
//
// The host's `TabsView` stays the source of truth for which tabs EXIST; this
// only says where each one sits. `reconcile` follows it: a tab the host no
// longer lists drops out, a new one lands in the focused group. The keys are
// the tab strip's own (`Tab.key`).
//
// Invariants every function keeps (`layout.test.ts` checks each after every
// step of a long random walk):
//   - every tab key sits in exactly one group;
//   - no group is empty, except the only one;
//   - a group's active key is one of its tabs, or null exactly when it is empty;
//   - a split has two or more children, none a split along its own axis, and
//     its ratios are positive and sum to 1;
//   - group ids are unique and never reused while the layout lives, and there
//     are at most `MAX_GROUPS` of them.
//
// A split therefore always moves a tab into its new group: there is no empty
// pane waiting for the host to open something. To open a session beside the
// current one, the host opens its tab (it lands in the focused group, shown
// once the host focuses it) and the window splits that tab out.

/** A group's id: `g<n>`, minted in order from `Layout.next`, so the same
 * steps always mint the same ids and a group keeps its id for its life. */
export type GroupId = string;

/** A leaf: an ordered strip of tab keys, one of them shown. */
export interface Group {
  readonly kind: "group";
  readonly id: GroupId;
  readonly tabs: readonly string[];
  readonly active: string | null;
}

/**
 * Which way a split lays its children out: `row` left to right (a split
 * right), `column` top to bottom (a split down). Named after CSS's
 * `flex-direction`, which is what draws it.
 */
export type Axis = "row" | "column";

/** An internal node: its children along `axis`, each taking `ratios[i]`. */
export interface Split {
  readonly kind: "split";
  readonly axis: Axis;
  readonly children: readonly LayoutNode[];
  readonly ratios: readonly number[];
}

export type LayoutNode = Group | Split;

export interface Layout {
  readonly root: LayoutNode;
  /** The group new tabs land in and the keyboard's pane is in. */
  readonly focused: GroupId;
  /** The number the next minted group id takes. */
  readonly next: number;
}

/** Where a split puts the new group: after the old one, along this axis. */
export type SplitDirection = "right" | "down";

/**
 * The most groups a layout holds. Far more panes than a window can show, and
 * a bound on what `parse` will walk: a stored tree is only as trustworthy as
 * the storage it came from.
 */
export const MAX_GROUPS = 64;

/** One group holding `keys`, in order, the first one shown. */
export function initialLayout(keys: readonly string[]): Layout {
  const tabs = [...new Set(keys)];
  return { root: { kind: "group", id: "g1", tabs, active: tabs[0] ?? null }, focused: "g1", next: 2 };
}

/** Every group, in reading order: depth first, so left before right and top
 * before bottom. This is the order `focusNext` walks. */
export function groups(layout: Layout): Group[] {
  return leaves(layout.root);
}

function leaves(node: LayoutNode): Group[] {
  return node.kind === "group" ? [node] : node.children.flatMap(leaves);
}

function findGroup(layout: Layout, id: GroupId): Group | undefined {
  return groups(layout).find((group) => group.id === id);
}

/** The id of the group holding `key`, or `null` when no group does. */
export function groupOf(layout: Layout, key: string): GroupId | null {
  return groups(layout).find((group) => group.tabs.includes(key))?.id ?? null;
}

/** `node` with each group passed through `edit`. */
function mapGroups(node: LayoutNode, edit: (group: Group) => LayoutNode): LayoutNode {
  if (node.kind === "group") return edit(node);
  return { ...node, children: node.children.map((child) => mapGroups(child, edit)) };
}

/** `group` without `key`. The tab shown next is the one after it, else the
 * one before: the neighbour the eye is already on, as a browser does. */
function withoutTab(group: Group, key: string): Group {
  const at = group.tabs.indexOf(key);
  if (at < 0) return group;
  const tabs = group.tabs.filter((tab) => tab !== key);
  const active = group.active === key ? (tabs[at] ?? tabs[at - 1] ?? null) : group.active;
  return { ...group, tabs, active };
}

/** The sibling that inherits from the child at `at` when it goes: the one
 * before it, else the one after. The one rule both a closing group's tabs
 * (`recipientOf`) and its space (`withoutGroup`) follow, so the two never
 * go to different siblings. */
function heirIndex(at: number): number {
  return at > 0 ? at - 1 : at + 1;
}

/**
 * The group a closing group hands its tabs to: its heir in the parent. When
 * that sibling is itself split, the group of it nearest the closing one: its
 * last group when it came before, its first when it came after.
 */
function recipientOf(node: LayoutNode, id: GroupId): GroupId | null {
  if (node.kind === "group") return null;
  const at = node.children.findIndex((child) => child.kind === "group" && child.id === id);
  if (at >= 0) {
    const heir = node.children[heirIndex(at)];
    if (heir === undefined) return null;
    const inHeir = leaves(heir);
    return (heirIndex(at) < at ? inHeir[inHeir.length - 1] : inHeir[0]).id;
  }
  for (const child of node.children) {
    const found = recipientOf(child, id);
    if (found !== null) return found;
  }
  return null;
}

/** `node` without the group `id`, its share of the split given to its heir.
 * The heir grows into the space: the whole subtree when it is split, the
 * rest of that split as it was. `null` when `node` was that group. */
function withoutGroup(node: LayoutNode, id: GroupId): LayoutNode | null {
  if (node.kind === "group") return node.id === id ? null : node;
  const kept = node.children.map((child) => withoutGroup(child, id));
  const gone = kept.indexOf(null);
  const ratios = [...node.ratios];
  if (gone >= 0) ratios[heirIndex(gone)] += ratios[gone];
  const children = kept.filter((child): child is LayoutNode => child !== null);
  if (children.length === 0) return null;
  return { ...node, children, ratios: ratios.filter((_, index) => kept[index] !== null) };
}

/**
 * `node` in its one canonical shape: a split of a single child is that child,
 * a split inside a split along the same axis is spliced into it (its ratios
 * scaled by its share), and every split's ratios are rescaled to sum to 1.
 * None of this moves anything on screen; it keeps two equal layouts equal.
 */
function tidy(node: LayoutNode): LayoutNode {
  if (node.kind === "group") return node;
  const children: LayoutNode[] = [];
  const ratios: number[] = [];
  node.children.forEach((raw, index) => {
    const child = tidy(raw);
    if (child.kind === "split" && child.axis === node.axis) {
      children.push(...child.children);
      ratios.push(...child.ratios.map((ratio) => ratio * node.ratios[index]));
    } else {
      children.push(child);
      ratios.push(node.ratios[index]);
    }
  });
  if (children.length === 1) return children[0];
  const sum = ratios.reduce((total, ratio) => total + ratio, 0);
  return { ...node, children, ratios: ratios.map((ratio) => ratio / sum) };
}

/** `layout` after a tab left `groupId`: the group closes into its sibling
 * when that left it empty, unless it is the last. */
function closeIfEmpty(layout: Layout, groupId: GroupId): Layout {
  return findGroup(layout, groupId)?.tabs.length === 0 ? closeGroup(layout, groupId) : layout;
}

/**
 * Split `groupId` in two: a new group to its right or below it, taking half
 * its space, the focus, and `movedTabKey` (the group's shown tab when none
 * is named), shown there.
 *
 * A split that would leave a group empty is not made: a group of one tab,
 * or of none, stays as it is. So does an unknown group, a `movedTabKey` the
 * group does not hold, and a layout already at `MAX_GROUPS`.
 */
export function splitGroup(layout: Layout, groupId: GroupId, direction: SplitDirection, movedTabKey?: string): Layout {
  const source = findGroup(layout, groupId);
  const moved = movedTabKey ?? source?.active;
  if (source === undefined || moved == null || source.tabs.length < 2 || !source.tabs.includes(moved)) return layout;
  if (groups(layout).length >= MAX_GROUPS) return layout;
  const id = `g${layout.next}`;
  const fresh: Group = { kind: "group", id, tabs: [moved], active: moved };
  const axis: Axis = direction === "right" ? "row" : "column";
  const root = mapGroups(layout.root, (group) =>
    group.id === groupId
      ? { kind: "split", axis, children: [withoutTab(source, moved), fresh], ratios: [0.5, 0.5] }
      : group,
  );
  return { root: tidy(root), focused: id, next: layout.next + 1 };
}

/**
 * Close `groupId`. Its tabs are not closed (the host owns those): they join
 * the end of the sibling group before it (after it, for a first child), which
 * also takes its space, and the focus when the closed group had it. The last
 * group cannot close.
 */
export function closeGroup(layout: Layout, groupId: GroupId): Layout {
  const closing = findGroup(layout, groupId);
  const recipient = recipientOf(layout.root, groupId);
  if (closing === undefined || recipient === null) return layout;
  const merged = mapGroups(layout.root, (group) =>
    group.id === recipient
      ? { ...group, tabs: [...group.tabs, ...closing.tabs], active: group.active ?? closing.active }
      : group,
  );
  const focused = layout.focused === groupId ? recipient : layout.focused;
  return { ...layout, root: tidy(withoutGroup(merged, groupId)!), focused };
}

/**
 * Move `key` to the end of `toGroup`, shown there, with the focus following
 * it: dragging a tab is choosing to look at it. A group the move empties
 * closes. Unknown key or group, or a move into the group it is already in,
 * returns `layout` as it was.
 */
export function moveTab(layout: Layout, key: string, toGroup: GroupId): Layout {
  const from = groupOf(layout, key);
  if (from === null || from === toGroup || findGroup(layout, toGroup) === undefined) return layout;
  const root = mapGroups(layout.root, (group) => {
    if (group.id === from) return withoutTab(group, key);
    if (group.id === toGroup) return { ...group, tabs: [...group.tabs, key], active: key };
    return group;
  });
  return closeIfEmpty({ ...layout, root, focused: toGroup }, from);
}

/**
 * Add `key` to the end of `groupId`, or of the focused group. It is shown
 * only when the group showed nothing yet, and the focus does not move: which
 * tab to show is the host's `TabsView.focus` to say (`activateTab`), so a tab
 * opened in the background stays there. A key already placed is moved
 * instead, as `moveTab` does, focus and all, so it never sits in two groups.
 * An unknown group returns `layout` as it was.
 */
export function addTab(layout: Layout, key: string, groupId: GroupId = layout.focused): Layout {
  if (findGroup(layout, groupId) === undefined) return layout;
  if (groupOf(layout, key) !== null) return moveTab(layout, key, groupId);
  const root = mapGroups(layout.root, (group) =>
    group.id === groupId ? { ...group, tabs: [...group.tabs, key], active: group.active ?? key } : group,
  );
  return { ...layout, root };
}

/**
 * Take `key` out of the layout: the host closed its tab. A group it leaves
 * empty closes into its sibling, unless it is the last group, which stays,
 * empty, for the next tab. An unknown key returns `layout` as it was.
 */
export function removeTab(layout: Layout, key: string): Layout {
  const from = groupOf(layout, key);
  if (from === null) return layout;
  const root = mapGroups(layout.root, (group) => (group.id === from ? withoutTab(group, key) : group));
  return closeIfEmpty({ ...layout, root }, from);
}

/** Show `key` in its group and focus that group: a click on its tab, or the
 * host focusing it. An unknown key returns `layout` as it was. */
export function activateTab(layout: Layout, key: string): Layout {
  const at = groupOf(layout, key);
  if (at === null) return layout;
  const root = mapGroups(layout.root, (group) => (group.id === at ? { ...group, active: key } : group));
  return { ...layout, root, focused: at };
}

/** Give `groupId` the focus. An unknown group returns `layout` as it was. */
export function focusGroup(layout: Layout, groupId: GroupId): Layout {
  if (findGroup(layout, groupId) === undefined) return layout;
  return { ...layout, focused: groupId };
}

/** Focus the group `step` places from the focused one in reading order,
 * wrapping, as `stepTab` does for tabs. */
function focusStep(layout: Layout, step: number): Layout {
  const all = groups(layout);
  const at = all.findIndex((group) => group.id === layout.focused);
  return focusGroup(layout, all[(at + step + all.length) % all.length].id);
}

/** Focus the next group in reading order, wrapping to the first. */
export function focusNext(layout: Layout): Layout {
  return focusStep(layout, 1);
}

/** Focus the previous group in reading order, wrapping to the last. */
export function focusPrevious(layout: Layout): Layout {
  return focusStep(layout, -1);
}

/**
 * `layout` following the host's tab list: a key `liveKeys` does not name
 * drops out (`removeTab`), a key it names that no group holds joins the
 * focused group (`addTab`), in `liveKeys` order. Calling it twice with the
 * same keys changes nothing the second time.
 */
export function reconcile(layout: Layout, liveKeys: readonly string[]): Layout {
  const live = new Set(liveKeys);
  let next = layout;
  for (const group of groups(layout)) {
    for (const key of group.tabs) if (!live.has(key)) next = removeTab(next, key);
  }
  for (const key of live) if (groupOf(next, key) === null) next = addTab(next, key);
  return next;
}

/** The layout's schema version, stored with it: a layout written by another
 * version is not guessed at, it starts over as one group. */
const VERSION = 1;

/** `layout` as a string `parse` reads back. */
export function serialize(layout: Layout): string {
  return JSON.stringify({ version: VERSION, root: layout.root, focused: layout.focused, next: layout.next });
}

/** A group id: at most nine digits, so every id and `next` stays an exact
 * integer and a minted id can never collide with a stored one. */
const GROUP_ID = /^g([1-9][0-9]{0,8})$/;
const MAX_NEXT = 1_000_000_000;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * `value` as a node when it is exactly one this module could have written,
 * else `null`. `ids` and `keys` collect what the tree already holds, so a
 * group id or a tab key seen twice rejects the whole tree rather than being
 * guessed at. A group must hold a tab unless it is `lone`, the whole tree:
 * the one group a layout lets be empty.
 */
function nodeFrom(value: unknown, ids: Set<string>, keys: Set<string>, lone = false): LayoutNode | null {
  if (!isRecord(value)) return null;
  if (value.kind === "group") {
    const { id, tabs, active } = value;
    if (typeof id !== "string" || !GROUP_ID.test(id) || ids.has(id) || ids.size >= MAX_GROUPS) return null;
    if (!Array.isArray(tabs) || !tabs.every((tab) => typeof tab === "string")) return null;
    if (tabs.some((tab) => keys.has(tab)) || new Set(tabs).size !== tabs.length) return null;
    if (tabs.length === 0 ? !lone || active !== null : typeof active !== "string" || !tabs.includes(active)) return null;
    ids.add(id);
    for (const tab of tabs) keys.add(tab);
    // The check above proved it; TypeScript does not narrow through `includes`.
    return { kind: "group", id, tabs, active: active as string | null };
  }
  if (value.kind === "split") {
    const { axis, children, ratios } = value;
    if (axis !== "row" && axis !== "column") return null;
    if (!Array.isArray(children) || !Array.isArray(ratios) || children.length === 0) return null;
    if (ratios.length !== children.length) return null;
    if (!ratios.every((ratio) => typeof ratio === "number" && Number.isFinite(ratio) && ratio > 0)) return null;
    // Each ratio finite is not enough: two near `Number.MAX_VALUE` sum to
    // Infinity, and `tidy` would rescale both to zero.
    if (!Number.isFinite(ratios.reduce((total: number, ratio: number) => total + ratio, 0))) return null;
    const nodes: LayoutNode[] = [];
    for (const child of children) {
      const node = nodeFrom(child, ids, keys);
      if (node === null) return null;
      nodes.push(node);
    }
    return { kind: "split", axis, children: nodes, ratios: ratios as number[] };
  }
  return null;
}

/**
 * The layout `raw` holds, when it is one `serialize` could have written; one
 * empty group for anything else: not JSON, another version, a repeated tab or
 * group, an empty group beside others, a focus on no group, more than
 * `MAX_GROUPS` groups, ratios that do not add up. Tabs are reconciled
 * afterwards (`readLayout`), so an empty start still shows every tab.
 */
export function parse(raw: string): Layout {
  try {
    const value: unknown = JSON.parse(raw);
    if (!isRecord(value) || value.version !== VERSION) return initialLayout([]);
    const { root, focused, next } = value;
    if (!Number.isSafeInteger(next) || (next as number) < 1 || (next as number) > MAX_NEXT) return initialLayout([]);
    const ids = new Set<string>();
    const tree = nodeFrom(root, ids, new Set(), true);
    if (tree === null || typeof focused !== "string" || !ids.has(focused)) return initialLayout([]);
    // Never mint an id the tree already holds, whatever `next` says.
    const highest = Math.max(...[...ids].map((id) => Number(GROUP_ID.exec(id)![1])));
    return { root: tidy(tree), focused, next: Math.max(next as number, highest + 1) };
  } catch {
    // A tree deep enough to overflow the walk is garbage like any other.
    return initialLayout([]);
  }
}

/** The smallest storage surface this module needs, so tests can pass a fake
 * (`sidebar_model.ts`'s `SidebarStorage` is the same shape, for the same
 * reason). */
export interface LayoutStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/** The localStorage key holding the window's tab-group layout. */
export const LAYOUT_KEY = "ainb.layout";

/**
 * The stored layout, following the host's `liveKeys`: tabs that closed while
 * the window was away drop out and new ones join the focused group. Storage
 * that is absent, throws (a private window, blocked site data) or holds
 * anything `parse` rejects gives one group holding every live tab.
 */
export function readLayout(storage: LayoutStorage | undefined, liveKeys: readonly string[]): Layout {
  let raw: string | null | undefined;
  try {
    raw = storage?.getItem(LAYOUT_KEY);
  } catch {
    raw = null;
  }
  return reconcile(raw ? parse(raw) : initialLayout([]), liveKeys);
}

/** Store `layout`; a storage that throws only loses the memory of it for
 * this window's life. */
export function writeLayout(storage: LayoutStorage | undefined, layout: Layout): void {
  try {
    storage?.setItem(LAYOUT_KEY, serialize(layout));
  } catch {
    // Nothing to do: the layout still applies for this window's life.
  }
}
