// The sidebar's ring and the header's counts, from the Sessions frame's merged
// attention.

import assert from "node:assert/strict";
import { test } from "node:test";
import type {
  AttentionKind,
  SessionStatus,
  Session_Serialize,
  SessionsView_Serialize,
} from "../../../ainb-app/bindings/AppState";
import { idleCount, isSelected, keyLabel, label, LABEL_CHARS, legacyTmuxSession, NEED_YOU, ringCount, ringFor, rowStatus } from "./sessions.ts";

function session(id: string, status: SessionStatus = "Running", marks: AttentionKind[] = []): Session_Serialize {
  return {
    id,
    name: id,
    workspace_path: "/repo",
    status,
    is_attached: false,
    attention: marks.map((kind) => ({ kind, detail: null })),
  } as unknown as Session_Serialize;
}

test("a row rings with the tightest kind of its merged attention", () => {
  assert.equal(ringFor(session("a", "Running", ["Done", "Approve", "Ask"])), "Ask");
  assert.equal(ringFor(session("b", "Running", ["Err", "Done"])), "Err");
  assert.equal(ringFor(session("c")), null, "no marks, no ring");
});

test("a row rings only from the frame: no status guess, no attention from another version", () => {
  // The host puts an error's ERR chip on the row itself; a status alone is not a ring.
  assert.equal(ringFor(session("d", { Error: "exited" })), null);
  const older = { id: "e", name: "e", status: "Idle" } as unknown as Session_Serialize;
  assert.equal(ringFor(older), null, "a host that sends no attention rings nothing");
});

test("header counts are per ring kind and idle status", () => {
  const sessions = {
    workspaces: [
      { name: "one", sessions: [session("a", "Running", ["Ask"]), session("b", "Idle")] },
      { name: "two", sessions: [session("c", "Idle", ["Ask", "Wait"]), session("d", { Error: "x" }, ["Err"])] },
    ],
  } as unknown as SessionsView_Serialize;
  assert.equal(ringCount(sessions, "Ask"), 2);
  assert.equal(ringCount(sessions, "Err"), 1);
  assert.equal(ringCount(sessions, "Wait"), 0);
  // "c" is Idle but rings Ask: it needs a person, so it is not idle as well.
  assert.equal(idleCount(sessions), 1);
});

test("a waiting session is counted once, in need-you, never also as idle", () => {
  // The driven run's two readings: one waiting session read "1 need you 1
  // idle", and with a second, idle session "1 need you 2 idle".
  const one = { workspaces: [{ name: "r", sessions: [session("w", "Idle", ["Ask"])] }] } as unknown as SessionsView_Serialize;
  assert.equal(ringCount(one, "Ask"), 1);
  assert.equal(idleCount(one), 0);
  const two = {
    workspaces: [{ name: "r", sessions: [session("w", "Idle", ["Ask"]), session("i", "Idle")] }],
  } as unknown as SessionsView_Serialize;
  assert.equal(ringCount(two, "Ask"), 1);
  assert.equal(idleCount(two), 1);
});

test("an idle session that only rings Done is idle, not left out of both counts", () => {
  // A finished turn is for reading: it is not a need-you kind, so the row
  // is counted idle rather than in neither total.
  const done = { workspaces: [{ name: "r", sessions: [session("d", "Idle", ["Done"])] }] } as unknown as SessionsView_Serialize;
  assert.deepEqual(
    NEED_YOU.map((kind) => ringCount(done, kind)),
    [0, 0, 0, 0],
  );
  assert.equal(idleCount(done), 1);
});

test("a label drops control and format characters and stops at the cap", () => {
  assert.equal(label("feat/\u202Eevil\u001b[31m\u200Bx"), "feat/evil[31mx");
  assert.equal(label("\u{1F600}".repeat(100)), "\u{1F600}".repeat(LABEL_CHARS));
});

test("the selected row is named by id, not by its place in the list", () => {
  // The frame carries only the rows the filter shows, so an index into the
  // reducer's full list would name the wrong one (#1180).
  const sessions = {
    workspaces: [{ name: "repo", sessions: [session("live"), session("boss")] }],
    selected_session_id: "boss",
  } as unknown as SessionsView_Serialize;
  assert.equal(isSelected(sessions, "boss"), true);
  assert.equal(isSelected(sessions, "live"), false);
  assert.equal(isSelected(undefined, "boss"), false);
});

test("a chip kind this build does not know is skipped, never ranked first", () => {
  const unknown = (marks: string[]) =>
    ({ ...session("u", "Idle"), attention: marks.map((kind) => ({ kind, detail: null })) }) as unknown as Session_Serialize;
  assert.equal(ringFor(unknown(["Info", "Done"])), "Done");
  assert.equal(ringFor(unknown(["Info"])), null);
});

test("a card no session row names reads as a name, never as its raw key", () => {
  // `SessionKey::legacy`, with the fingerprint the daemon really writes
  // (`pane=%N;pid=N;session_started=N`, `discover/tmux.rs`): the
  // tmux target, which may itself carry `:`.
  assert.equal(keyLabel("legacy:claude:hangar-dev:1.0:pane=%3;pid=41;session_started=1790000000"), "hangar-dev:1.0");
  assert.equal(legacyTmuxSession("legacy:claude:hangar-dev:1.0:pane=%3;pid=41;session_started=1790000000"), "hangar-dev");
  assert.equal(legacyTmuxSession("claude:5f0c9a1e"), null);
  assert.equal(keyLabel("legacy:claude:tmux:"), "tmux");
  assert.equal(keyLabel("claude:5f0c9a1e-77aa-4c1d-9e4b-000000000000"), "claude 5f0c9a1e");
  assert.equal(keyLabel("bare"), "bare");
});

test("a status this build does not know is unknown, never guessed as stopped", () => {
  assert.equal(rowStatus("Paused" as unknown as SessionStatus), "unknown");
  assert.equal(rowStatus("Stopped"), "stopped");
});

test("a lifecycle this build does not know is warned about once, not on every render", () => {
  const warn = console.warn;
  const seen: string[] = [];
  console.warn = (message: string) => seen.push(message);
  try {
    for (let i = 0; i < 3; i += 1) rowStatus("Hibernating" as unknown as SessionStatus);
  } finally {
    console.warn = warn;
  }
  assert.equal(seen.filter((message) => message.includes("Hibernating")).length, 1);
});
