// What the sidebar draws, projected from the Sessions frame into Orca's
// project-and-worktree shape. Projections only, in the manner of `board.ts`:
// `sidebar.tsx` draws what these return and keeps nothing of its own.
//
//   Workspace_Serialize (a project)  ──sessions[]──▶ ProjectGroup
//   Session_Serialize.workspace_path ──folds by path──▶ WorktreeCard
//   WorktreeCard.sessions[]          ──one row each──▶ agent rows (sidebar.tsx)
//
// `workspace_path` on `Session_Serialize` is the git WORKTREE path, not the
// project root (`crates/ainb-app/src/models/session.rs:639`, set from
// `WorktreeManager`): several sessions can share one worktree, each its own
// agent (or a shell). `Workspace_Serialize.path` is the project root that
// groups them. Folding by `workspace_path` is what turns two sessions in one
// worktree into one card with two agent rows, which is the point of this
// module.

import type {
  GitChanges,
  SessionAgentType,
  SessionLabelStore,
  Session_Serialize,
  SessionsView_Serialize,
} from "../../../ainb-app/bindings/AppState";

/** One worktree, several sessions may share it. */
export interface WorktreeCard {
  /** The worktree path, or a session id when the frame omits one (an older
   * host, a test fixture): stable across a drain either way, so `For` never
   * rebuilds the card just because the frame refreshed. */
  key: string;
  /** The primary session's display name, or its name: `session.rs` calls
   * `display_name` the operator's own label, overriding the auto-generated
   * one when set. A frame leaves `display_name` off a session row (#983 M19),
   * so the label comes from the `session_labels` section, keyed by the row's
   * tmux session name (`titleOf`). */
  title: string;
  /** The session the title names: what a rename of the card renames. */
  primaryId: string;
  /** The primary session's branch. */
  branch: string;
  /** The primary session's dirty counts, or `null` when there is nothing to
   * show: no changes, or a host that never sent any. */
  gitChanges: GitChanges | null;
  /** The primary session's model, or `null` when the host did not set one. */
  model: string | null;
  /** Every session in this worktree, in the frame's own order: one agent row
   * each. Never re-sorted, for the same reason `allSessions` in `sessions.ts`
   * is not: the frame already carries only the rows the filter shows. */
  sessions: Session_Serialize[];
}

/** One project: `Workspace_Serialize`'s own name and path, plus its cards. */
export interface ProjectGroup {
  name: string;
  path: string;
  /** How many sessions the project holds, across every worktree: the group
   * header's count, not the number of cards. */
  sessionCount: number;
  cards: WorktreeCard[];
}

/** `session.created_at` as epoch millis, or `-Infinity` when it is missing
 * or unparsable: loses to any dated session rather than throwing, which is
 * what "unknown fields tolerated" means for a timestamp. */
function createdAt(session: Session_Serialize): number {
  const parsed = Date.parse(session.created_at ?? "");
  return Number.isNaN(parsed) ? -Infinity : parsed;
}

/** The session a card's title, branch, git counts and model come from: the
 * newest of the sessions sharing the worktree. A tie, or every session
 * missing the field, keeps the first, so ties never reorder the frame's own
 * order either. */
function primaryOf(sessions: readonly Session_Serialize[]): Session_Serialize {
  let primary = sessions[0];
  let primaryAt = createdAt(primary);
  for (const session of sessions.slice(1)) {
    const at = createdAt(session);
    if (at > primaryAt) {
      primary = session;
      primaryAt = at;
    }
  }
  return primary;
}

/** What a session's card title reads: its label in `labels` (the host's
 * label store, by tmux session name), else a `display_name` the row carries
 * itself, else its name. The host checks a rename against the same order
 * (`ainb-desktop/src/rename.rs`, `shown`). */
export function titleOf(session: Session_Serialize, labels: SessionLabelStore | undefined): string {
  const tmux = session.tmux_session_name;
  return (tmux ? labels?.[tmux] : undefined) ?? session.display_name ?? session.name;
}

function cardFor(key: string, sessions: Session_Serialize[], labels: SessionLabelStore | undefined): WorktreeCard {
  const primary = primaryOf(sessions);
  const changes = primary.git_changes;
  const dirty = changes && (changes.added > 0 || changes.modified > 0 || changes.deleted > 0) ? changes : null;
  return {
    key,
    title: titleOf(primary, labels),
    primaryId: primary.id,
    branch: primary.branch_name,
    gitChanges: dirty,
    model: primary.model ?? null,
    sessions,
  };
}

/**
 * `sessions` folded into one card per worktree path, in the frame's own order
 * (a card sits where its first session does). Never re-sorted: the host's
 * "Select next session" and "Select previous session" walk the frame's order,
 * so a card drawn anywhere else moves the selection somewhere the eye does not
 * expect, or nowhere at all from what looks like the first row.
 *
 * The host sorts that order recent first and keeps a worktree's sessions
 * together (`sort_recent_first` in `ainb-app`'s `models/workspace.rs`), so
 * folding by path never reorders it: the cards are newest first, and what the
 * host walks is what is drawn. It sorts by creation, not access, so opening a
 * session never moves its card under the pointer.
 *
 * `fallbackPath` is the project's own path, for a session that carries no
 * `workspace_path` of its own (an older host, a test fixture):
 * every such session in one project folds into the SAME card rather than
 * one each, which is the closest a missing field can get to the real shape.
 */
export function worktreeCards(
  sessions: readonly Session_Serialize[],
  fallbackPath: string,
  labels?: SessionLabelStore,
): WorktreeCard[] {
  const byPath = new Map<string, Session_Serialize[]>();
  for (const session of sessions) {
    const key = session.workspace_path || fallbackPath;
    const bucket = byPath.get(key);
    if (bucket) bucket.push(session);
    else byPath.set(key, [session]);
  }
  return [...byPath.entries()].map(([key, group]) => cardFor(key, group, labels));
}

/**
 * The Sessions frame as the sidebar draws it: one group per project, in the
 * frame's order, each holding the worktree cards its sessions fold
 * into. A project with no sessions is dropped, matching the frame's own rule
 * that an empty workspace draws nothing (`sidebar.test.ts`). `labels` names
 * each card as `titleOf` does.
 */
export function projectGroups(view: SessionsView_Serialize | undefined, labels?: SessionLabelStore): ProjectGroup[] {
  return (view?.workspaces ?? [])
    .filter((workspace) => workspace.sessions.length > 0)
    .map((workspace) => ({
      name: workspace.name,
      path: workspace.path,
      sessionCount: workspace.sessions.length,
      cards: worktreeCards(workspace.sessions, workspace.path, labels),
    }));
}

/**
 * The session Orca's `worktree.navigateUp` / `worktree.navigateDown` shows: the
 * worktree card `step` places from the one holding `selected`, in sidebar
 * order and wrapping, and that card's own session (the one its title names).
 * Cards in a `collapsed` project are not drawn, so they are skipped, as Orca
 * skips them (`worktree-keyboard-cycle.ts:47-48`). With no drawn card holding
 * `selected` it enters from the end the step points away from
 * (`worktree-keyboard-cycle.ts:87-92`). `null` with no cards.
 */
export function stepWorktree(
  view: SessionsView_Serialize | undefined,
  selected: string | null,
  step: 1 | -1,
  collapsed: ReadonlySet<string> = new Set(),
): string | null {
  const cards = projectGroups(view)
    .filter((group) => !collapsed.has(group.path))
    .flatMap((group) => group.cards);
  if (cards.length === 0) return null;
  const at = cards.findIndex((card) => card.sessions.some((session) => session.id === selected));
  const next = at < 0 ? cards[step > 0 ? 0 : cards.length - 1] : cards[(at + step + cards.length) % cards.length];
  return primaryOf(next.sessions).id;
}

/** `+added ~modified -deleted`, each part only while it is non-zero: a card
 * with nothing dirty draws no line at all (`sidebar.tsx` gates on `null`). */
export function formatGitChanges(changes: GitChanges): string {
  const parts: string[] = [];
  if (changes.added > 0) parts.push(`+${changes.added}`);
  if (changes.modified > 0) parts.push(`~${changes.modified}`);
  if (changes.deleted > 0) parts.push(`-${changes.deleted}`);
  return parts.join(" ");
}

/** The agent row's label for a provider: the wire's own enum value, except
 * `Ssh`, which reads as the initialism. */
const AGENT_LABELS: Record<SessionAgentType, string> = {
  Claude: "Claude",
  Shell: "Shell",
  Ssh: "SSH",
  Codex: "Codex",
  Gemini: "Gemini",
  Copilot: "Copilot",
  Antigravity: "Antigravity",
  Kiro: "Kiro",
};

/** `session.agent_type`'s label; a value this build does not know (a host at
 * another version) falls back to the wire string itself rather than throwing. */
export function agentLabel(agentType: SessionAgentType): string {
  return AGENT_LABELS[agentType] ?? agentType;
}

/** The smallest storage surface this module needs, so tests can pass a fake
 * (`theme.ts`'s `ThemeStorage` is the same shape, for the same reason). */
export interface SidebarStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/** The localStorage key holding which project paths are collapsed. */
export const COLLAPSED_KEY = "ainb.sidebar.collapsed";

/**
 * Which project paths are collapsed, from storage (a path, not a name: two
 * repositories can share a directory name); empty when storage is
 * absent, throws (a private window, blocked site data), or holds something
 * that is not the array this module wrote.
 */
export function readCollapsed(storage: SidebarStorage | undefined): ReadonlySet<string> {
  try {
    const raw = storage?.getItem(COLLAPSED_KEY);
    if (!raw) return new Set();
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? new Set(parsed.filter((entry): entry is string => typeof entry === "string")) : new Set();
  } catch {
    return new Set();
  }
}

/** Store which project paths are collapsed; a storage that throws only loses
 * the memory of it for this window's life. */
export function writeCollapsed(storage: SidebarStorage | undefined, collapsed: ReadonlySet<string>): void {
  try {
    storage?.setItem(COLLAPSED_KEY, JSON.stringify([...collapsed]));
  } catch {
    // Nothing to do: the collapse still applies for this window's life.
  }
}
