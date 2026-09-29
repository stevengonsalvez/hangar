// The empty work area's content: the selected session or the prompt to choose.

import assert from "node:assert/strict";
import { test } from "node:test";
import type { SessionsView_Serialize } from "../../../ainb-app/bindings/AppState";
import { emptyPaneView } from "./pane_empty.ts";

function view(selected: string | null): SessionsView_Serialize {
  return {
    selected_session_id: selected,
    workspaces: [
      {
        name: "repo",
        sessions: [
          { id: "s-1", name: "fix login", branch_name: "ainb/fix-login", status: "Running" },
          { id: "s-2", name: "docs", branch_name: "ainb/docs", status: "Idle" },
        ],
      },
    ],
  } as unknown as SessionsView_Serialize;
}

test("with a row selected, the empty pane names that session, not 'choose one'", () => {
  // A closed tab leaves its row selected (#193): the pane must agree with it.
  assert.deepEqual(emptyPaneView(view("s-2")), {
    kind: "selected",
    sessionId: "s-2",
    name: "docs",
    branch: "ainb/docs",
  });
});

test("with nothing selected, or a selection the frame no longer lists, it asks to choose", () => {
  assert.deepEqual(emptyPaneView(view(null)), { kind: "choose" });
  assert.deepEqual(emptyPaneView(view("gone")), { kind: "choose" });
  assert.deepEqual(emptyPaneView(undefined), { kind: "choose" });
});

test("with the workspace shell selected, no session is chosen, whatever id the frame names", () => {
  // The same reading of the selection as the answer banner's (`selectedSession`):
  // a shell selection is not a session, so there is none to open.
  const shell = { ...view("s-1"), shell_selected: true } as unknown as SessionsView_Serialize;
  assert.deepEqual(emptyPaneView(shell), { kind: "choose" });
});
