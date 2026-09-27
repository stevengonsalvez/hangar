// What the work area shows when no terminal tab is open: the session the
// sidebar has selected, with a way back into its terminal, or the prompt to
// choose one when nothing is selected.
//
// Closing a tab detaches the window from its session; the session runs on and
// its row stays selected. Saying "Choose a session" beside a row that is
// visibly chosen made the two halves of the window disagree (#193).

import type { SessionsView_Serialize } from "../../../ainb-app/bindings/AppState";
import { allSessions, label } from "./sessions.ts";

export type EmptyPane =
  | { kind: "choose" }
  | { kind: "selected"; sessionId: string; name: string; branch: string };

/** The empty work area's content for the Sessions frame `view`. */
export function emptyPaneView(view: SessionsView_Serialize | undefined): EmptyPane {
  const selected = view?.selected_session_id ?? null;
  const session = selected === null ? undefined : allSessions(view).find((row) => row.id === selected);
  if (session === undefined) return { kind: "choose" };
  return { kind: "selected", sessionId: session.id, name: label(session.name), branch: label(session.branch_name) };
}
