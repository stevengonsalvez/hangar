// The window's side of the tab-group layout (`layout.ts`): following the
// host's tab strip, and where a dragged tab lands. Pure, like the model it
// drives, so `panes.test.ts` checks each rule without a window.
//
//   host TabsView ──▶ followHost ──▶ Layout ──▶ geometry ──▶ panes.tsx
//   drag  (x, y)  ──▶ dropAt ──▶ Drop ──▶ dropTab ──▶ Layout

import {
  activateTab,
  focusGroup,
  geometry,
  groups,
  moveTab,
  reconcile,
  splitGroup,
  type GroupId,
  type Layout,
  type SplitDirection,
} from "./layout.ts";
import { tabAfterClose } from "./tabs.ts";

/**
 * `before` following the host's `liveKeys`: tabs that closed drop out, new
 * ones join the focused group (`reconcile`). A group whose shown tab closed
 * shows the tab Orca would (`tabAfterClose`): the one of its own shown most
 * recently (`recent`, oldest first), else its neighbour. The focus stays
 * where `reconcile` left it. Runs on every tab-strip answer, so nothing to
 * follow returns `before` itself, and the window neither redraws nor
 * stores anything.
 */
export function followHost(before: Layout, liveKeys: readonly string[], recent: readonly string[]): Layout {
  let next = reconcile(before, liveKeys);
  if (next === before) return before;
  const live = new Set(liveKeys);
  const was = new Map(groups(before).map((group) => [group.id, group]));
  for (const group of groups(next)) {
    const old = was.get(group.id);
    if (old === undefined || old.active === null || live.has(old.active)) continue;
    const keyed = (keys: readonly string[]) => keys.map((key) => ({ key }));
    const shown = tabAfterClose(keyed(old.tabs), keyed(group.tabs), recent, old.active);
    if (shown !== null && shown !== group.active) next = focusGroup(activateTab(next, shown), next.focused);
  }
  return next;
}

/** A box in window pixels, as `getBoundingClientRect` gives it. */
export interface Box {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** Where on a pane's body a drop lands: its middle joins the group, an edge
 * splits the group that way. */
export type DropZone = "center" | SplitDirection;

/** Where a dragged tab would land: in a group's strip before `before` (the
 * end when `null`), or on a group's body. */
export type Drop =
  | { kind: "strip"; group: GroupId; before: string | null }
  | { kind: "body"; group: GroupId; zone: DropZone };

/**
 * The zone of `body` under `point`, as Orca resolves it
 * (`orca:src/renderer/src/components/tab-group/tab-drop-zone.ts:8-37`): the
 * middle, inside a tenth of each edge, joins the group; outside it the
 * left and right thirds split sideways and the middle third up or down.
 */
export function dropZone(body: Box, point: { x: number; y: number }): DropZone {
  const x = point.x - body.left;
  const y = point.y - body.top;
  const edgeX = body.width * 0.1;
  const edgeY = body.height * 0.1;
  if (x > edgeX && x < body.width - edgeX && y > edgeY && y < body.height - edgeY) return "center";
  const third = body.width / 3;
  if (x < third) return "left";
  if (x > third * 2) return "right";
  return y < body.height / 2 ? "up" : "down";
}

/**
 * Where a tab dragged to `point` lands in `layout`, drawn in `window` with
 * each group's strip `stripPx` tall: over a strip, before `hovered` (the tab
 * under the pointer) when that tab is in the strip's group; over a body, in
 * the zone `dropZone` names. `null` off every group.
 */
export function dropAt(
  layout: Layout,
  window: Box,
  point: { x: number; y: number },
  stripPx: number,
  hovered: string | null,
): Drop | null {
  for (const [group, rect] of geometry(layout).groups) {
    const box = {
      left: window.left + rect.x * window.width,
      top: window.top + rect.y * window.height,
      width: rect.w * window.width,
      height: rect.h * window.height,
    };
    if (point.x < box.left || point.x >= box.left + box.width || point.y < box.top || point.y >= box.top + box.height) {
      continue;
    }
    if (point.y < box.top + stripPx) {
      const mine = groups(layout).find((one) => one.id === group)?.tabs.includes(hovered ?? "") ?? false;
      return { kind: "strip", group, before: mine ? hovered : null };
    }
    const body = { ...box, top: box.top + stripPx, height: Math.max(0, box.height - stripPx) };
    return { kind: "body", group, zone: dropZone(body, point) };
  }
  return null;
}

/**
 * `layout` with `key` dropped at `drop`: into a strip at the hovered tab's
 * place, onto a body's middle to join that group, onto a body's edge to
 * split that group with `key` in the new half. A drop that would change
 * nothing, or split a group of `key` alone (Orca refuses the same,
 * `orca:src/renderer/src/components/tab-bar/tab-move-to-pane-column.ts:26-28`),
 * returns `layout` as it was.
 */
export function dropTab(layout: Layout, key: string, drop: Drop): Layout {
  const target = groups(layout).find((group) => group.id === drop.group);
  if (target === undefined) return layout;
  if (drop.kind === "strip") {
    // Dropped on itself: it stays where it is.
    if (drop.before === key) return layout;
    const rest = target.tabs.filter((tab) => tab !== key);
    const at = drop.before === null ? rest.length : rest.indexOf(drop.before);
    return moveTab(layout, key, drop.group, at < 0 ? rest.length : at);
  }
  if (drop.zone === "center") return moveTab(layout, key, drop.group);
  if (target.tabs.includes(key)) return splitGroup(layout, drop.group, drop.zone, key);
  const moved = moveTab(layout, key, drop.group);
  const split = splitGroup(moved, drop.group, drop.zone, key);
  // A split the layout refuses (every group already drawn) leaves the tab
  // where it was, rather than dropped into the middle it was not aimed at.
  if (split === moved) return layout;
  // The tab only passed through the group it split: that group still shows
  // what it showed.
  return target.active === null ? split : focusGroup(activateTab(split, target.active), split.focused);
}
