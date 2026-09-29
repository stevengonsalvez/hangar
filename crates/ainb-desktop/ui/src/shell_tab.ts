// A plain terminal in a worktree, opened the way Orca opens one: from the tab
// strip or Mod+T, in the worktree in front of the person
// (`orca:src/renderer/src/components/tab-bar/tab-create-menu-options.ts:88-91`).
// The window names the selected session, never a folder: the host finds that
// session's worktree and asks the daemon for the shell (`shell_open`), and
// the tab arrives on the strip like every other. Closing a shell's tab ends
// the shell (`shell_close`), as Orca's close kills its PTY
// (`orca:src/renderer/src/store/terminals/terminal-tab-close-providers.ts:33`).

import { invoke } from "@tauri-apps/api/core";
import { openRowIntent, rowOf, type Tab, type TabTarget } from "./tabs.ts";

/** What a press says when there is no worktree to open in. */
export const NO_SESSION = "Select a session first: a new terminal opens in its worktree.";

export interface ShellTabDeps {
  /** The session whose worktree a new terminal opens in: the sidebar's selection. */
  session(): string | null;
  toast(message: string): void;
}

/**
 * Open and close for the tab strip. A refusal comes back from the host as
 * the sentence to show (chosen there by the daemon's error code, with its
 * detail), and is shown as it came.
 */
export function createShellTabs(deps: ShellTabDeps) {
  /** A new shell in the selected session's worktree; the host opens its tab. */
  const open = async (): Promise<void> => {
    const session = deps.session();
    if (session === null) {
      deps.toast(NO_SESSION);
      return;
    }
    try {
      await invoke<string>("shell_open", { session });
    } catch (error) {
      deps.toast(String(error));
    }
  };
  /** Close `tab`: a shell's tab ends its shell, any other only detaches. */
  const close = (tab: Tab): void => {
    if (tab.target.kind !== "shell") {
      void invoke("terminal_close", { key: tab.key });
      return;
    }
    invoke("shell_close", { key: tab.key }).catch((error: unknown) => deps.toast(String(error)));
  };
  return { open, close };
}

/**
 * Re-attach a detached tab. A session or tmux tab goes through its
 * session-list row, so the reducer marks it attached; a shell has no row, so
 * the host re-attaches it directly.
 */
export function reattach(tab: Tab): void {
  if (tab.target.kind === "shell") void invoke("shell_reattach", { key: tab.key });
  else void invoke("dispatch", { intent: openRowIntent(rowOf(tab.target)) });
}

/** A shell tab's title: "Terminal" and its worktree's folder name. */
export function shellTitle(target: Extract<TabTarget, { kind: "shell" }>): string {
  const folder = target.dir.replace(/\/+$/, "").split("/").pop();
  return folder ? `Terminal · ${folder}` : "Terminal";
}
