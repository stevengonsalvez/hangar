// `sidebar_model.ts` holds no store and no DOM, so it is tested here as plain
// functions: build a frame by hand, call the projection, assert the shape.

import { test } from "node:test";
import assert from "node:assert/strict";
import type { Session_Serialize, SessionsView_Serialize, Workspace_Serialize } from "../../../ainb-app/bindings/AppState";
import {
  agentLabel,
  formatGitChanges,
  projectGroups,
  readCollapsed,
  worktreeCards,
  writeCollapsed,
} from "./sidebar_model.ts";

/** A minimal session: only the fields a fixture below actually varies, the
 * same shape `sidebar.test.ts` and `sessions.test.ts` already fixture with. */
function session(over: Partial<Session_Serialize> & { id: string }): Session_Serialize {
  return {
    name: over.id,
    workspace_path: "",
    branch_name: `ainb/${over.id}`,
    container_id: null,
    status: "Running",
    created_at: "2024-01-01T00:00:00Z",
    last_accessed: "2024-01-01T00:00:00Z",
    git_changes: { added: 0, modified: 0, deleted: 0 },
    recent_logs: null,
    skip_permissions: false,
    mode: "Interactive",
    boss_prompt: null,
    agent_type: "Claude",
    model: null,
    codex_model: null,
    ssh_target: null,
    display_name: null,
    tmux_session_name: null,
    preview_content: null,
    is_attached: false,
    attention: [],
    ...over,
  } as Session_Serialize;
}

function workspace(over: Partial<Workspace_Serialize> & { name: string; path: string }): Workspace_Serialize {
  return { sessions: [], shell_session: null, ...over };
}

test("two sessions in one worktree fold into one card with two agent rows", () => {
  const cards = worktreeCards(
    [
      session({ id: "claude-1", workspace_path: "/repo/wt-a", agent_type: "Claude" }),
      session({ id: "shell-1", workspace_path: "/repo/wt-a", agent_type: "Shell" }),
      session({ id: "claude-2", workspace_path: "/repo/wt-b", agent_type: "Claude" }),
    ],
    "/repo",
  );
  assert.equal(cards.length, 2, "one card per distinct worktree path, not per session");
  const [a] = cards.filter((card) => card.key === "/repo/wt-a");
  assert.equal(a.sessions.length, 2);
  assert.deepEqual(a.sessions.map((row) => row.id), ["claude-1", "shell-1"], "frame order, not resorted");
});

test("a session with no workspace_path falls back to the project path, folding with its siblings", () => {
  const cards = worktreeCards(
    [session({ id: "old-host-1" }), session({ id: "old-host-2" })],
    "/repo",
  );
  assert.equal(cards.length, 1, "both sessions fold into the project's own path");
  assert.equal(cards[0].key, "/repo");
  assert.equal(cards[0].sessions.length, 2);
});

test("cards sort newest created first", () => {
  const cards = worktreeCards(
    [
      session({ id: "stale", workspace_path: "/repo/old", created_at: "2024-01-01T00:00:00Z" }),
      session({ id: "fresh", workspace_path: "/repo/new", created_at: "2024-06-01T00:00:00Z" }),
    ],
    "/repo",
  );
  assert.deepEqual(cards.map((card) => card.key), ["/repo/new", "/repo/old"]);
});

test("a session missing created_at sorts to the bottom, never throws", () => {
  const cards = worktreeCards(
    [
      session({ id: "no-clock", workspace_path: "/repo/unknown", created_at: undefined as unknown as string }),
      session({ id: "dated", workspace_path: "/repo/dated", created_at: "2024-06-01T00:00:00Z" }),
    ],
    "/repo",
  );
  assert.deepEqual(cards.map((card) => card.key), ["/repo/dated", "/repo/unknown"]);
});

test("a card's title, branch and model come from its newest session", () => {
  const [card] = worktreeCards(
    [
      session({
        id: "older",
        workspace_path: "/repo/wt",
        created_at: "2024-01-01T00:00:00Z",
        branch_name: "ainb/older",
        model: "haiku",
      }),
      session({
        id: "newer",
        workspace_path: "/repo/wt",
        created_at: "2024-06-01T00:00:00Z",
        branch_name: "ainb/newer",
        model: "opus",
        display_name: "Fix the flake",
      }),
    ],
    "/repo",
  );
  assert.equal(card.title, "Fix the flake");
  assert.equal(card.branch, "ainb/newer");
  assert.equal(card.model, "opus");
});

test("zero git counts hide: a card with nothing dirty carries no gitChanges", () => {
  const [clean] = worktreeCards([session({ id: "clean", workspace_path: "/repo/a" })], "/repo");
  assert.equal(clean.gitChanges, null);

  const [dirty] = worktreeCards(
    [session({ id: "dirty", workspace_path: "/repo/b", git_changes: { added: 2, modified: 0, deleted: 1 } })],
    "/repo",
  );
  assert.deepEqual(dirty.gitChanges, { added: 2, modified: 0, deleted: 1 });
});

test("formatGitChanges shows only the non-zero parts, added/modified/deleted in order", () => {
  assert.equal(formatGitChanges({ added: 3, modified: 0, deleted: 0 }), "+3");
  assert.equal(formatGitChanges({ added: 3, modified: 2, deleted: 1 }), "+3 ~2 -1");
  assert.equal(formatGitChanges({ added: 0, modified: 0, deleted: 0 }), "");
});

test("projectGroups drops a project with no sessions and counts the rest", () => {
  const view: SessionsView_Serialize = {
    workspaces: [
      workspace({ name: "repo", path: "/repo", sessions: [session({ id: "a" }), session({ id: "b" })] }),
      workspace({ name: "empty", path: "/empty", sessions: [] }),
    ],
    selected_workspace_index: 0,
    selected_session_id: null,
    shell_selected: false,
    selected_sessions: [],
    expand_all_workspaces: false,
    session_filter: "active_only",
    attached_session_id: null,
    favorite_workspace_paths: [],
  };
  const groups = projectGroups(view);
  assert.deepEqual(groups.map((group) => group.name), ["repo"]);
  assert.equal(groups[0].sessionCount, 2);
});

test("projectGroups tolerates an undefined frame: no groups, no throw", () => {
  assert.deepEqual(projectGroups(undefined), []);
});

test("agentLabel names every provider the wire enum has, and falls back for one it does not", () => {
  assert.equal(agentLabel("Claude"), "Claude");
  assert.equal(agentLabel("Ssh"), "SSH");
  assert.equal(agentLabel("unknown-provider" as never), "unknown-provider");
});

class FakeStorage {
  private readonly data = new Map<string, string>();
  getItem(key: string): string | null {
    return this.data.get(key) ?? null;
  }
  setItem(key: string, value: string): void {
    this.data.set(key, value);
  }
}

class ThrowingStorage {
  getItem(): string | null {
    throw new Error("blocked site data");
  }
  setItem(): void {
    throw new Error("blocked site data");
  }
}

test("collapsed projects round-trip through storage", () => {
  const storage = new FakeStorage();
  assert.deepEqual(readCollapsed(storage), new Set());
  writeCollapsed(storage, new Set(["repo-a", "repo-b"]));
  assert.deepEqual(readCollapsed(storage), new Set(["repo-a", "repo-b"]));
});

test("collapsed projects read as empty, and write as a no-op, when storage is absent or throws", () => {
  assert.deepEqual(readCollapsed(undefined), new Set());
  assert.deepEqual(readCollapsed(new ThrowingStorage()), new Set());
  assert.doesNotThrow(() => writeCollapsed(new ThrowingStorage(), new Set(["x"])));
});

test("opening a session does not move its card: the order is by creation", () => {
  const sessions = [
    session({ id: "old", workspace_path: "/repo/old", created_at: "2024-01-01T00:00:00Z", last_accessed: "2024-09-01T00:00:00Z" }),
    session({ id: "new", workspace_path: "/repo/new", created_at: "2024-06-01T00:00:00Z", last_accessed: "2024-06-01T00:00:00Z" }),
  ];
  assert.deepEqual(
    worktreeCards(sessions, "/repo").map((card) => card.key),
    ["/repo/new", "/repo/old"],
  );
});
