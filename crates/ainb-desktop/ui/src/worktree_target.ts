// Which worktree the strip's "+" means: the shown tab's, since the control
// sits above it, as Orca's "+" acts on the active worktree
// (`orca:src/renderer/src/components/tab-bar/tab-create-menu-options.ts:88-91`).
// The page names it by ids only; the host resolves the folder
// (`crates/ainb-desktop/src/worktree_target.rs`).

import type { WorktreeTarget } from "../../bindings/Desktop.ts";
import type { Tab, TabTarget } from "./tabs.ts";

export type { WorktreeTarget };

/**
 * The tab the work area shows: `undefined` when no terminal is shown (a
 * board or a page is), `null` for a shown terminal pane with no tab, else
 * that tab's target.
 */
export function shownTargetOf(showingTerminal: boolean, tabs: readonly Tab[], active: string | null): TabTarget | null | undefined {
  if (!showingTerminal) return undefined;
  return tabs.find((tab) => tab.key === active)?.target ?? null;
}

/**
 * The worktree `shown` (`shownTargetOf`) names: a session tab its session, a
 * shell tab itself (the host knows its folder), a bare tmux tab nothing, as
 * it has no worktree. With no tab shown, the sidebar's selection.
 */
export function worktreeTarget(shown: TabTarget | null | undefined, selected: string | null): WorktreeTarget | null {
  switch (shown?.kind) {
    case "session":
      return { kind: "session", id: shown.id };
    case "shell":
      return { kind: "shell", key: shown.tmux };
    case "tmux":
      return null;
    default:
      return selected === null ? null : { kind: "session", id: selected };
  }
}
