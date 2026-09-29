// The sidebar row's context menu, as data: which items a row offers, which of
// them it can take, and how the keyboard walks them. `row_menu.tsx` draws what
// these return; `sidebar.tsx` opens it and hands a pick to `main.tsx`.
//
// Orca's worktree context menu (stablyai/orca@31012ae,
// `src/renderer/src/components/sidebar/WorktreeContextMenuView.tsx:157-388`)
// has more items than this. Only the ones this window already has an action
// for are here: an existing reducer command or host command, nothing new.

import type { Session_Serialize, SessionsView_Serialize } from "../../../ainb-app/bindings/AppState";
import { allSessions } from "./sessions.ts";
import { selectRowIntent, type RendererIntent } from "./tabs.ts";

/** What a menu item does when chosen. */
export type RowMenuAction = "open" | "editor" | "copy_path" | "copy_name" | "delete";

export interface RowMenuItem {
  action: RowMenuAction;
  label: string;
  /** Drawn but not choosable: this row cannot take the action. */
  disabled: boolean;
  /** Why it is disabled, shown as the item's tooltip. */
  reason?: string;
  /** Drawn as Orca's destructive item: it removes something. */
  destructive?: boolean;
}

const NO_PATH = "This session has no worktree path";
const LOCAL_ONLY = "Local only: this session runs on a remote host";

/**
 * The items for `session`'s row, in Orca's order: Open in, Copy Path, Copy
 * Worktree Name, and Delete last, destructive (stablyai/orca@3c1af16,
 * `WorktreeContextMenuView.tsx:344-387`). Open leads, as the row's own click
 * does.
 *
 * A row with no worktree path has nothing to open or copy as a path. A remote
 * row's path is on another machine, so a local editor cannot open it: Orca's
 * Open in marks the same case "Local only" (`WorktreeOpenInMenu.tsx:63`).
 * Delete runs the terminal's delete on this machine, so a remote row cannot
 * take it either, nor a Boss row, whose container only the terminal removes.
 */
export function rowMenuItems(session: Session_Serialize): RowMenuItem[] {
  const hasPath = session.workspace_path !== "";
  const remote = session.ssh_target != null;
  const editorReason = !hasPath ? NO_PATH : remote ? LOCAL_ONLY : undefined;
  const deleteReason = remote ? LOCAL_ONLY : session.mode === "Boss" ? "A Boss session is deleted from the terminal" : undefined;
  return [
    { action: "open", label: "Open", disabled: false },
    { action: "editor", label: "Open in Editor", disabled: editorReason !== undefined, reason: editorReason },
    { action: "copy_path", label: "Copy Path", disabled: !hasPath, reason: hasPath ? undefined : NO_PATH },
    { action: "copy_name", label: "Copy Worktree Name", disabled: false },
    { action: "delete", label: "Delete", disabled: deleteReason !== undefined, reason: deleteReason, destructive: true },
  ];
}

/**
 * The next enabled item from `at` in `delta`'s direction, wrapping at either
 * end, or -1 when none is enabled. From -1 (nothing focused yet) down lands on
 * the first enabled item and up on the last.
 */
export function stepItem(items: readonly RowMenuItem[], at: number, delta: 1 | -1): number {
  const count = items.length;
  let index = at < 0 ? (delta === 1 ? -1 : count) : at;
  for (let tried = 0; tried < count; tried += 1) {
    index = (index + delta + count) % count;
    if (!items[index].disabled) return index;
  }
  return -1;
}

interface KeyLike {
  key: string;
  shiftKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  metaKey: boolean;
}

/**
 * Whether `event` is the keyboard's way to a context menu: the context-menu
 * key, or Shift+F10. Handled by hand because WebKit, the webview on macOS and
 * Linux, raises no `contextmenu` event for either.
 */
export function opensRowMenu(event: KeyLike): boolean {
  if (event.ctrlKey || event.altKey || event.metaKey) return false;
  return event.key === "ContextMenu" || (event.key === "F10" && event.shiftKey);
}

/**
 * Open the row's worktree in the configured editor: select the row without
 * attaching it, then the session list's own `o` row, which reads the
 * selection. Sent in order, so the editor opens this row and not the last.
 */
export function editorIntents(sessionId: string): RendererIntent[] {
  return [selectRowIntent({ session: sessionId }), { Command: ["session_list.editor", null] }];
}

/** Session `id` as `view` has it now, or `undefined` once it has left. */
export function sessionIn(view: SessionsView_Serialize | undefined, id: string): Session_Serialize | undefined {
  return allSessions(view).find((session) => session.id === id);
}

/** An item chosen on a row: what to do, to which session, and the worktree's
 * name as the card shows it (the card's title, not the session's own name). */
export interface RowPick {
  action: RowMenuAction;
  session: Session_Serialize;
  name: string;
}

/** The window's existing actions a pick maps onto. */
export interface RowPickDeps {
  /** The row's own click: select and attach its session. */
  open(sessionId: string): void;
  /** Dispatch reducer intents in order (`main.tsx`'s `run`). */
  run(intents: RendererIntent[]): void;
  /** The host's `clipboard_write`. */
  copy(text: string): void;
  /** The intent that puts the session list's selection back on the row of
   * the terminal the window shows, or nothing when it shows none. */
  reselect(): RendererIntent[];
  /** Open the delete confirmation on the row (`delete_dialog.ts`). Nothing
   * is deleted until it is confirmed there. */
  confirmDelete(pick: RowPick): void;
}

/** Run `pick` through the one existing action that serves it. */
export function runRowPick(pick: RowPick, deps: RowPickDeps): void {
  switch (pick.action) {
    case "open":
      return deps.open(pick.session.id);
    case "editor":
      // The selection scopes the answer banner (`questionOver`): left on this
      // row, it would hide the shown terminal's own question.
      return deps.run([...editorIntents(pick.session.id), ...deps.reselect()]);
    case "copy_path":
      return deps.copy(pick.session.workspace_path);
    case "copy_name":
      return deps.copy(pick.name);
    case "delete":
      return deps.confirmDelete(pick);
  }
}
