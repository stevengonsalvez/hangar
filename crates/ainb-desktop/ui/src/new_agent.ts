// "New agent here": one more agent in the worktree the window shows, as
// Orca's tab-bar "+" launches a picked agent into the active worktree
// (`orca:src/renderer/src/components/tab-bar/QuickLaunchButton.tsx`).
//
//   WorktreeTarget + agent ──invoke("worktree_agent_add")──▶ host: ids ──own state──▶ worktree
//                                                           host ──worktree/agent_add──▶ daemon
//   CreatedWorktree ◀── host opens + focuses the tab ──┘ then `createFollow` selects its row
//
// The page names a listed session or an open shell tab, never a path: the
// host resolves the folder itself (`worktree_target.ts`, and
// `crates/ainb-desktop/src/worktree_target.rs` on the host).

import { invoke } from "@tauri-apps/api/core";
import { createSignal, type Accessor } from "solid-js";
import type { AddAgentArgs, CreatedWorktree, SpawnAgent } from "../../bindings/Desktop.ts";
import { createFollow, type FollowDeps } from "./composer.ts";
import type { WorktreeTarget } from "./worktree_target.ts";
/** What the flow needs from the window it runs in. */
export interface NewAgentDeps extends FollowDeps {
  /** The host call. Absent: the real `worktree_agent_add` command. */
  add?(args: AddAgentArgs): Promise<CreatedWorktree>;
  /** Where a refusal goes: the host's sentence, chosen by the daemon's code. */
  toast(message: string): void;
}

export interface NewAgentFlow {
  /** Whether an add is on the host: the control is off until it answers. */
  adding: Accessor<boolean>;
  /** Add `agent` to the worktree `target` names, unless an add is running. */
  add(target: WorktreeTarget, agent: SpawnAgent): void;
}

/** The flow the window runs, in one place so the window and its test run the
 * same code. */
export function createNewAgentFlow(deps: NewAgentDeps): NewAgentFlow {
  const add = deps.add ?? ((args: AddAgentArgs) => invoke<CreatedWorktree>("worktree_agent_add", { args }));
  const follow = createFollow(deps);
  const [adding, setAdding] = createSignal(false);
  return {
    adding,
    add(target, agent) {
      if (adding()) return;
      setAdding(true);
      add({ target, agent }).then(
        (created) => {
          setAdding(false);
          follow(created.session_id);
        },
        (error: unknown) => {
          setAdding(false);
          deps.toast(String(error));
        },
      );
    },
  };
}
