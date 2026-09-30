// The sidebar row menu's Delete, as data and state: what the dialog says for
// what the delete would remove, and the flow from a pick to the host's delete.
// `delete_dialog.tsx` draws it; `main.tsx` owns one flow.
//
// Ported from Orca's delete confirmation (stablyai/orca@3c1af16,
// `src/renderer/src/components/sidebar/DeleteWorktreeDialog.tsx`). Orca's
// workspace is a worktree; here a row is one session in a worktree several
// sessions may share, so what goes depends on the host's preview:
//
//   removed  the worktree goes with the session: Orca's own copy and hint
//   shared   another session still works in it: only this session goes
//   kept     a folder ainb did not make: only the session goes, as Orca's
//            folder workspace (`delete-worktree-dialog-copy.ts:58-60`)

import { invoke } from "@tauri-apps/api/core";
import { createSignal } from "solid-js";
import type { DeletePreview, TreeFate } from "../../bindings/Desktop.ts";
import type { RowPick } from "./row_menu.ts";

export type { DeletePreview, TreeFate };

/** Where the host's answer about what the delete removes stands. */
export type PreviewState =
  | { kind: "checking" }
  | { kind: "ready"; preview: DeletePreview }
  | { kind: "failed"; reason: string };

/** What the dialog says. The description is Orca's `Remove <name> <suffix>`
 * (`DeleteWorktreeDialogDescription.tsx:18-30`), in three parts so the name
 * can be set apart. */
export interface DeleteCopy {
  title: string;
  before: string;
  target: string;
  after: string;
  confirm: string;
  /** Whether the confirm button may be pressed yet. */
  ready: boolean;
  /** Dirty or uncounted work goes with the tree: Orca's Force Delete
   * (`DeleteWorktreeDialogFooter.tsx:39-40`), and Cancel takes the keyboard
   * so Enter never wipes it by reflex. */
  force: boolean;
}

/** Whether deleting now takes work: a tree that goes holds uncommitted
 * changes, or git could not count them. */
export function forcesDelete(state: PreviewState): boolean {
  return (
    state.kind === "ready" &&
    state.preview.tree === "removed" &&
    (state.preview.changes === null || state.preview.changes > 0)
  );
}

/** Orca's dirty line (`DeleteWorktreeDirtyChangeHint.tsx:15-18`) for
 * `changes` files, or `null` when there are none. */
export function dirtyHint(changes: number | null): string | null {
  if (changes === null || changes <= 0) return null;
  return `${changes} uncommitted or untracked ${changes === 1 ? "change" : "changes"}`;
}

/** The dirty line's tooltip, Orca's (`DeleteWorktreeDirtyChangeHint.tsx:31`). */
export const DIRTY_HINT_TITLE = "Deleting this workspace permanently removes these changes from disk.";

/** Said when git could not count the worktree's changes: unknown, not clean. */
export const UNCHECKED_HINT = "Could not check this worktree for uncommitted changes.";

/** What the dialog says about deleting the worktree named `name`. */
export function deleteCopy(name: string, state: PreviewState): DeleteCopy {
  const removed = {
    title: "Delete Workspace",
    before: "Remove ",
    target: name,
    after: " from git and delete its workspace folder.",
    confirm: "Delete Workspace",
    ready: true,
    force: false,
  };
  if (state.kind === "checking") {
    return { ...removed, title: "Delete", before: "Checking what deleting ", after: " removes…", confirm: "Delete", ready: false };
  }
  // Unknown is not confirmable: the host could not say whether the folder
  // is shared or holds uncommitted work, and its delete checks the same
  // answer again, so it would refuse anyway.
  if (state.kind === "failed") {
    return { ...removed, title: "Delete", after: ". What it removes could not be checked.", confirm: "Delete", ready: false };
  }
  switch (state.preview.tree) {
    case "removed":
      return forcesDelete(state) ? { ...removed, confirm: "Force Delete", force: true } : removed;
    case "shared":
      return {
        ...removed,
        title: "Delete Session",
        before: "Remove this session from ",
        after: ". Another session still works in this worktree, so its folder will not be deleted.",
        confirm: "Delete Session",
      };
    case "kept":
      return {
        ...removed,
        title: "Delete Session",
        after: " from ainb. The folder on disk will not be deleted.",
        confirm: "Delete Session",
      };
  }
}

/** The line under the target: dirty, unchecked, or nothing. Only a removed
 * tree loses files, so only it is counted. */
export function changesLine(state: PreviewState): { text: string; dirty: boolean } | null {
  if (state.kind !== "ready" || state.preview.tree !== "removed") return null;
  const dirty = dirtyHint(state.preview.changes);
  if (dirty !== null) return { text: dirty, dirty: true };
  return state.preview.changes === null ? { text: UNCHECKED_HINT, dirty: false } : null;
}

/** The row the dialog is open on. */
export interface DeleteTarget {
  sessionId: string;
  /** The worktree's name as its card shows it. */
  name: string;
  /** The worktree path, shown under the name as Orca does. */
  path: string;
}

/** What the person confirmed, sent with the delete: the host counts again
 * right before removing, and refuses if the fate moved or new changes
 * appeared (a live agent still writing), forced or not. */
export interface Confirmed {
  expected: TreeFate;
  /** The changes the dialog showed, `null` when uncounted. */
  expectedChanges: number | null;
  /** The person pressed Force Delete: shown dirty or uncounted work may go. */
  force: boolean;
}

export interface DeleteFlowDeps {
  /** The host's `session_delete_preview`. */
  preview?(sessionId: string): Promise<DeletePreview>;
  /** The host's `session_delete`: the terminal's own delete, refused by the
   * host unless what it removes is still what the dialog said. */
  remove?(sessionId: string, confirmed: Confirmed): Promise<void>;
  /** Say a failed delete. */
  toast(text: string): void;
}

export interface DeleteFlow {
  target(): DeleteTarget | null;
  state(): PreviewState;
  /** Open the dialog on a picked row and ask the host what goes. */
  open(pick: RowPick): void;
  /** Delete, once, and close: the dialog gets out of the way as the delete
   * starts, as Orca's does (`DeleteWorktreeDialog.tsx:290-292`). */
  confirm(): void;
  close(): void;
}

export function createDeleteFlow(deps: DeleteFlowDeps): DeleteFlow {
  const preview = deps.preview ?? ((id: string) => invoke<DeletePreview>("session_delete_preview", { sessionId: id }));
  const remove =
    deps.remove ??
    ((id: string, confirmed: Confirmed) => invoke<void>("session_delete", { sessionId: id, ...confirmed }));
  const [target, setTarget] = createSignal<DeleteTarget | null>(null);
  const [state, setState] = createSignal<PreviewState>({ kind: "checking" });
  // Each opening's own token: a late answer for an earlier opening is dropped.
  let opening = 0;

  return {
    target,
    state,
    open(pick) {
      const token = ++opening;
      const sessionId = pick.session.id;
      // The state first: the dialog is drawn as the target is set, and must
      // not draw, or focus by, the last opening's answer.
      setState({ kind: "checking" });
      setTarget({ sessionId, name: pick.name, path: pick.session.workspace_path });
      preview(sessionId).then(
        (answer) => token === opening && setState({ kind: "ready", preview: answer }),
        (error: unknown) => token === opening && setState({ kind: "failed", reason: String(error) }),
      );
    },
    confirm() {
      const at = target();
      const now = state();
      if (at === null || now.kind !== "ready") return;
      opening += 1;
      setTarget(null);
      const confirmed: Confirmed = {
        expected: now.preview.tree,
        expectedChanges: now.preview.changes,
        force: forcesDelete(now),
      };
      remove(at.sessionId, confirmed).catch((error: unknown) => deps.toast(String(error)));
    },
    close() {
      opening += 1;
      setTarget(null);
    },
  };
}
