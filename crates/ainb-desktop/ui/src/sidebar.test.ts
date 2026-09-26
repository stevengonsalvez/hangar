// The sidebar rendered over a Sessions frame draws exactly the frame's
// sessions, one `.session-row` per session, each still a single clickable
// element the specs and `sidebar_model.test.ts`'s grouping agree on: the
// frame order within a worktree, never re-filtered here (#1180). Rendered for
// real, through Vite's SSR loader and the Solid plugin, so a filter re-added
// in `sidebar.tsx` fails here.

import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import solid from "vite-plugin-solid";

const session = (id: string, workspacePath: string, status: unknown = "Running") => ({
  id,
  name: id,
  workspace_path: workspacePath,
  branch_name: `ainb/${id}`,
  status,
  agent_type: "Claude",
  git_changes: { added: 0, modified: 0, deleted: 0 },
  attention: [],
});

test("the sidebar draws exactly the frame's sessions, and folds a shared worktree into one card", async () => {
  const frame = {
    workspaces: [
      {
        name: "repo",
        path: "/repo",
        // "live" and "gone" share a worktree: one card, two agent rows. "boss"
        // is its own worktree, its own card.
        sessions: [
          session("live", "/repo/wt-a"),
          session("gone", "/repo/wt-a", "Stopped"),
          session("boss", "/repo/wt-b", "Stopped"),
        ],
      },
      { name: "empty", path: "/empty", sessions: [] },
    ],
    selected_workspace_index: 0,
    selected_session_id: "boss",
    // The TUI's own filter reads active_only; the window must not apply it.
    session_filter: "active_only",
  };
  const server = await createServer({
    configFile: false,
    root: fileURLToPath(new URL("..", import.meta.url)),
    plugins: [solid({ ssr: true })],
    server: { middlewareMode: true, hmr: false },
    appType: "custom",
    // Vite resolves Solid itself, so the component and `renderToString` share
    // one server build whatever conditions the test runner was started with.
    ssr: { noExternal: ["solid-js"] },
    logLevel: "silent",
  });
  try {
    const { Sidebar } = await server.ssrLoadModule("/src/sidebar.tsx");
    const { renderToString } = await server.ssrLoadModule("solid-js/web");
    const html: string = renderToString(() =>
      Sidebar({ sessions: frame, stale: false, loading: false, onOpen() {}, ref() {} }),
    );
    const drawn = [...html.matchAll(/data-session="([^"]+)"/g)].map((match) => match[1]);
    assert.deepEqual(drawn, ["live", "gone", "boss"], html);
    assert.equal(html.match(/class="worktree-card"/g)?.length, 2, "two worktrees, two cards");
    assert.equal(html.match(/aria-current="true"/g)?.length, 1, html);
    assert.match(html, /data-workspace="repo"/);
    assert.doesNotMatch(html, /data-workspace="empty"/, "a workspace with no rows is not drawn");
  } finally {
    await server.close();
  }
});
