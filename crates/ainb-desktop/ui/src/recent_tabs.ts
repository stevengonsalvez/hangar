// The recent-tab switcher (Ctrl+Tab) and reopen closed tab (Cmd+Shift+T), as
// Orca has them (orca a0abcb68):
//
//   Ctrl+Tab held ──▶ the focused pane's tabs, most recently shown first
//     Tab / Shift+Tab ──▶ step on / back       Ctrl up ──▶ show it
//     Esc, or the window losing focus ──▶ nothing changes
//   a tab a person closes ──▶ closed stack (10, newest first)
//     Cmd+Shift+T ──▶ newest entry that can come back, where it was
//
// Pure, like `layout.ts`: `recent_tabs.test.ts` checks each rule without a window.

import type { SessionsView_Serialize } from "../../../ainb-app/bindings/AppState";
import { groupOf, groups, moveTab, type GroupId, type Layout } from "./layout.ts";
import { allSessions } from "./sessions.ts";
import type { RowId, Tab, TabTarget } from "./tabs.ts";
import type { WorktreeTarget } from "./worktree_target.ts";

/**
 * The switcher's list for a pane holding `paneTabs`: `shown` first, then the
 * pane's other tabs most recently shown first (`recent`, oldest first), then
 * any never shown, in strip order. Orca orders its switcher the same way
 * (`src/renderer/src/components/tab-bar/recent-tab-switching.ts:112-150`), so
 * the first Ctrl+Tab toggles back to the tab shown before.
 */
export function switcherOrder(paneTabs: readonly string[], recent: readonly string[], shown: string | null): string[] {
  const inPane = new Set(paneTabs);
  const ordered = [...recent].reverse().filter((key) => inPane.has(key));
  for (const key of paneTabs) if (!ordered.includes(key)) ordered.push(key);
  if (shown !== null && inPane.has(shown)) return [shown, ...ordered.filter((key) => key !== shown)];
  return ordered;
}

/**
 * The entry `step` places from `at` in a list of `count`, wrapping. From no
 * entry (`at` < 0) a step on starts at the first and a step back at the last,
 * as Orca's does (`recent-tab-switching.ts:191-203`).
 */
export function stepIndex(count: number, at: number, step: 1 | -1): number {
  if (count <= 0) return -1;
  if (at < 0) return step > 0 ? 0 : count - 1;
  return (at + step + count) % count;
}

/**
 * Whether `event` lets go of the held Ctrl, which shows the chosen tab. A
 * Tab keyup with Ctrl already up counts too: Orca commits on it, since some
 * surfaces report the last release that way
 * (`src/shared/window-shortcut-policy.ts:114-125`).
 */
export function releasesSwitcher(event: { type: string; key: string; ctrlKey: boolean }): boolean {
  if (event.type !== "keyup") return false;
  return event.key === "Control" || (event.key === "Tab" && !event.ctrlKey);
}

/** How many closed tabs are kept: Orca's
 * `MAX_RECENT_CLOSED_TERMINAL_TABS` (`src/renderer/src/store/slices/recently-closed-tabs.ts:37`). */
export const MAX_CLOSED = 10;

/** A tab a person closed: what it showed, and where it sat. */
export interface ClosedTab {
  key: string;
  target: TabTarget;
  /** The pane it was in, and its place in that pane's strip; `null` when no
   * pane held it. */
  group: GroupId | null;
  index: number;
}

/** `tab` as it is closed out of `layout`. */
export function closedFrom(layout: Layout, tab: Tab): ClosedTab {
  const group = groupOf(layout, tab.key);
  const index = groups(layout).find((one) => one.id === group)?.tabs.indexOf(tab.key) ?? -1;
  return { key: tab.key, target: tab.target, group, index };
}

/** `stack` (newest first) with `closed` on top, at most `MAX_CLOSED` long. */
export function pushClosed(stack: readonly ClosedTab[], closed: ClosedTab): ClosedTab[] {
  return [closed, ...stack].slice(0, MAX_CLOSED);
}

/** What the host is asked to bring a closed tab back: its session-list row
 * reattached, or a fresh shell in the worktree a closed shell was in. */
export type Reopen = { kind: "row"; row: RowId } | { kind: "shell"; target: WorktreeTarget };

/**
 * What brings `closed` back, or `null` when nothing can. Closing a session or
 * tmux tab only detached it, so its row reattaches it, the same terminal: a
 * session that left the list cannot. Closing a shell's tab ended the shell,
 * so, as Orca reopens a terminal as a fresh shell in its old folder and never
 * its old process (`recently-closed-tabs.ts:24-26`), a new one opens there,
 * named by a listed session whose worktree it is, or an open shell tab in
 * it: the host takes ids, never a path. A tab already open again is not
 * closed.
 */
export function reopenOf(closed: ClosedTab, open: readonly Tab[], sessions: SessionsView_Serialize | undefined): Reopen | null {
  if (open.some((tab) => tab.key === closed.key)) return null;
  const target = closed.target;
  switch (target.kind) {
    case "session":
      return allSessions(sessions).some((row) => row.id === target.id) ? { kind: "row", row: { session: target.id } } : null;
    case "tmux":
      return { kind: "row", row: { other_tmux: target.tmux } };
    case "shell": {
      const session = allSessions(sessions).find((row) => row.workspace_path === target.dir);
      if (session !== undefined) return { kind: "shell", target: { kind: "session", id: session.id } };
      const shell = open.find((tab) => tab.target.kind === "shell" && tab.target.dir === target.dir);
      return shell === undefined ? null : { kind: "shell", target: { kind: "shell", key: shell.key } };
    }
  }
}

/**
 * The newest entry of `stack` that can come back and how, and the stack
 * without it. Entries above it that cannot come back are dropped, as Orca
 * drops a drained entry and tries the next (`recently-closed-tabs.ts:169-201`).
 */
export function popReopenable(
  stack: readonly ClosedTab[],
  open: readonly Tab[],
  sessions: SessionsView_Serialize | undefined,
): { found: { closed: ClosedTab; reopen: Reopen } | null; rest: ClosedTab[] } {
  for (let at = 0; at < stack.length; at += 1) {
    const reopen = reopenOf(stack[at], open, sessions);
    if (reopen !== null) return { found: { closed: stack[at], reopen }, rest: stack.slice(at + 1) };
  }
  return { found: null, rest: [] };
}

/**
 * `layout` with each reopened tab it now holds moved back to the pane and
 * place it closed from (`placing`, by the key it came back as), as Orca puts
 * a reopened tab back in its group at its index
 * (`src/renderer/src/store/slices/recently-closed-tab-position.ts:119-167`),
 * and the keys it placed. A pane closed since leaves the tab where it landed.
 */
export function placeReopened(layout: Layout, placing: ReadonlyMap<string, ClosedTab>): { layout: Layout; placed: string[] } {
  let next = layout;
  const placed: string[] = [];
  for (const [key, closed] of placing) {
    if (groupOf(next, key) === null) continue;
    placed.push(key);
    if (closed.group !== null && closed.index >= 0) next = moveTab(next, key, closed.group, closed.index);
  }
  return { layout: next, placed };
}
