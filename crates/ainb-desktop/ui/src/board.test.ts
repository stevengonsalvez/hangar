// The board's columns and the attention list, from the frames the window holds.

import assert from "node:assert/strict";
import { test } from "node:test";
import type {
  AgentCardFrame,
  AgentStatusView,
  FleetView_Serialize,
  SessionsView_Serialize,
} from "../../../ainb-app/bindings/AppState";
import { ackTurn, NO_ACKS, type AckMap } from "./acks.ts";
import { agentStateCounts, attentionRows, boardColumns, boardHealth, COLUMNS, elsewhereCount, showIntents } from "./board.ts";

function card(sessionKey: string, over: Partial<AgentCardFrame> = {}): AgentCardFrame {
  return {
    session_key: sessionKey,
    provider: "claude",
    lifecycle: "RUNNING",
    transport_health: "HEALTHY",
    state: "working",
    has_open_request: false,
    wait_kind: null,
    turn_complete: false,
    tier: "hook",
    ...over,
  } as AgentCardFrame;
}

function status(...cards: AgentCardFrame[]): AgentStatusView {
  return {
    absent: null,
    head_revision: 3,
    view: {
      host_id: "local",
      read_revision: 3,
      received_at_ms: 1,
      head_revision: 3,
      health: { kind: "live" },
      cards,
    },
  };
}

/** A sessions frame with one row per `[id, name, provider id]`, and the Fleet
 * metadata that correlates each row to its provider session. */
function world(...rows: [string, string, string][]): {
  sessions: SessionsView_Serialize;
  fleet: FleetView_Serialize;
} {
  return {
    sessions: {
      workspaces: [{ name: "repo", sessions: rows.map(([id, name]) => ({ id, name, attention: [] })) }],
    } as unknown as SessionsView_Serialize,
    fleet: {
      fleet_metadata: Object.fromEntries(rows.map(([id, , provider]) => [id, { provider_session_id: provider }])),
      fleet_snapshot: [{ session_key: "claude:p-1", model: "opus" }],
      daemon_attention: { by_session_id: {}, all: {}, reachable: true, error: null, not_running: false },
      attention_elsewhere: 0,
    } as unknown as FleetView_Serialize,
  };
}

test("the columns are Orca's own words, never the raw AgentState", () => {
  // A card the host calls idle still reads idle whatever its lifecycle says,
  // and a card the host calls waiting lands in "needs": the renderer maps
  // through `status.ts` and never re-derives a state of its own.
  const { sessions, fleet } = world(["u-1", "api", "p-1"]);
  const columns = boardColumns(
    status(card("claude:p-1", { state: "idle", lifecycle: "RUNNING" }), card("codex:p-2", { state: "waiting" })),
    fleet,
    sessions,
    NO_ACKS,
  );
  assert.deepEqual(
    columns.map((column) => column.state),
    COLUMNS,
  );
  const by = Object.fromEntries(columns.map((column) => [column.state, column.cards.map((c) => c.key)]));
  assert.deepEqual(by.idle, ["claude:p-1"]);
  assert.deepEqual(by.working, []);
  assert.deepEqual(by.needs, ["codex:p-2"]);
});

test("a needs-you card carries the need kind its wait_kind names", () => {
  const { sessions, fleet } = world();
  const [needs] = boardColumns(
    status(
      card("a:1", { state: "waiting", wait_kind: "ask" }),
      card("b:2", { state: "waiting", wait_kind: "approval" }),
      card("c:3", { state: "waiting", wait_kind: null }),
    ),
    fleet,
    sessions,
    NO_ACKS,
  ).filter((column) => column.state === "needs");
  const by = Object.fromEntries(needs.cards.map((c) => [c.key, c.status]));
  assert.deepEqual(by["a:1"], { kind: "needs", need: "ask" });
  assert.deepEqual(by["b:2"], { kind: "needs", need: "approve" });
  assert.deepEqual(by["c:3"], { kind: "needs", need: "wait" }, "no wait_kind still needs a human, by default wait");
});

test("an idle card whose turn just finished reads done until this viewer acks it", () => {
  const { sessions, fleet } = world();
  const agentStatus = status(card("a:1", { state: "idle", turn_complete: true, evidence_observed_at: 7 }));
  const before = boardColumns(agentStatus, fleet, sessions, NO_ACKS);
  assert.deepEqual(
    before.map((column) => [column.state, column.cards.map((c) => c.key)]),
    [
      ["needs", []],
      ["working", []],
      ["done", ["a:1"]],
      ["idle", []],
    ],
  );

  const acked: AckMap = ackTurn(NO_ACKS, "a:1", 7);
  const after = boardColumns(agentStatus, fleet, sessions, acked);
  const by = Object.fromEntries(after.map((column) => [column.state, column.cards.map((c) => c.key)]));
  assert.deepEqual(by.done, [], "acked at this turn: the Done card is gone");
  assert.deepEqual(by.idle, ["a:1"], "and the card reads idle instead");
});

test("a later turn's Done shows again after an earlier turn was acked", () => {
  const { sessions, fleet } = world();
  const acked = ackTurn(NO_ACKS, "a:1", 7);
  const laterTurn = status(card("a:1", { state: "idle", turn_complete: true, evidence_observed_at: 8 }));
  const columns = boardColumns(laterTurn, fleet, sessions, acked);
  const by = Object.fromEntries(columns.map((column) => [column.state, column.cards.map((c) => c.key)]));
  assert.deepEqual(by.done, ["a:1"], "turn 8 was never acked, only turn 7 was");
});

test("an exited agent's card is hidden, not a fifth column", () => {
  const { sessions, fleet } = world();
  const columns = boardColumns(status(card("a:1", { state: "exited" })), fleet, sessions, NO_ACKS);
  assert.deepEqual(
    columns.flatMap((column) => column.cards.map((c) => c.key)),
    [],
  );
});

test("an unverifiable card falls in with idle, marked unverifiable rather than drawn as plainly idle", () => {
  const { sessions, fleet } = world();
  const columns = boardColumns(status(card("a:1", { state: "unverifiable" })), fleet, sessions, NO_ACKS);
  const [idle] = columns.filter((column) => column.state === "idle");
  assert.deepEqual(idle.cards.map((c) => c.status), [{ kind: "unverifiable" }]);
});

test("a working card with an Err attention chip still needs a human", () => {
  const sessions = {
    workspaces: [{ name: "repo", sessions: [{ id: "u-1", name: "api", attention: [{ kind: "Err", detail: null }] }] }],
  } as unknown as SessionsView_Serialize;
  const fleet = {
    fleet_metadata: { "u-1": { provider_session_id: "p-1" } },
    fleet_snapshot: [],
    daemon_attention: { by_session_id: {}, all: {}, reachable: true, error: null, not_running: false },
    attention_elsewhere: 0,
  } as unknown as FleetView_Serialize;
  const columns = boardColumns(status(card("claude:p-1", { state: "working" })), fleet, sessions, NO_ACKS);
  const by = Object.fromEntries(columns.map((column) => [column.state, column.cards.map((c) => c.key)]));
  assert.deepEqual(by.needs, ["claude:p-1"]);
  assert.deepEqual(by.working, []);
});

test("a card takes its row's name and the fleet's model, and one with no row still draws", () => {
  const { sessions, fleet } = world(["u-1", "api", "p-1"]);
  const [working] = boardColumns(status(card("claude:p-1"), card("claude:p-9")), fleet, sessions, NO_ACKS).filter(
    (column) => column.state === "working",
  );
  const known = working.cards.find((c) => c.key === "claude:p-1")!;
  assert.equal(known.title, "api");
  assert.equal(known.sessionId, "u-1");
  assert.equal(known.model, "opus");
  const stray = working.cards.find((c) => c.key === "claude:p-9");
  assert.ok(stray, "an agent the sidebar has not listed is still on the board");
  assert.equal(stray.sessionId, null);
  assert.equal(stray.title, "claude p-9", "named by provider and id, not the raw key");
});

test("an agent with something open floats to the top of its column", () => {
  const { sessions, fleet } = world();
  const [needs] = boardColumns(
    status(
      card("a:1", { state: "waiting" }),
      card("b:2", { state: "waiting", has_open_request: true }),
    ),
    fleet,
    sessions,
    NO_ACKS,
  ).filter((column) => column.state === "needs");
  assert.deepEqual(
    needs.cards.map((c) => c.key),
    ["b:2", "a:1"],
  );
});

test("an absent, stale or unreachable status draws its health, not an empty board", () => {
  assert.equal(boardHealth(undefined).kind, "absent");
  assert.deepEqual(boardHealth({ absent: "no daemon", head_revision: 0, view: null }), {
    kind: "absent",
    detail: "no daemon",
  });
  const stale = status();
  stale.view!.health = { kind: "stale", read_revision: 2, head_revision: 5 };
  assert.deepEqual(boardHealth(stale), { kind: "stale", behind: 3 });
  const gone = status();
  gone.view!.health = { kind: "unreachable", stale_since_ms: 1, reason: "socket closed" };
  assert.deepEqual(boardHealth(gone), { kind: "unreachable", reason: "socket closed" });
  assert.deepEqual(boardHealth(status()), { kind: "live" });
});

test("the attention list is the daemon's open rows, tightest first, titled by their session", () => {
  const { sessions, fleet } = world(["u-1", "api", "p-1"]);
  fleet.daemon_attention.by_session_id = {
    "p-1": [{ kind: "Err", detail: "exited 1" }],
    "p-unknown": [{ kind: "Ask", detail: "which branch?" }],
  } as unknown as FleetView_Serialize["daemon_attention"]["by_session_id"];
  const rows = attentionRows(fleet, sessions);
  assert.deepEqual(
    rows.map((row) => [row.kind, row.title, row.sessionId]),
    [
      ["Ask", "p-unknown", null],
      ["Err", "api", "u-1"],
    ],
  );
});

test("rows waiting elsewhere are counted, never swallowed", () => {
  const { fleet } = world();
  fleet.attention_elsewhere = 2;
  assert.equal(elsewhereCount(fleet), 2);
  assert.equal(elsewhereCount(undefined), 0);
});

test("a click selects the row without attaching it, then shows the pane it is about", () => {
  assert.deepEqual(showIntents("u-1", true), [
    { Command: ["session_list.select_row", { target: { session: "u-1" }, open: false }] },
    { Command: ["session_list.select_tab", { tab: "Ask" }] },
  ]);
  assert.deepEqual(showIntents("u-1", false)[1], { Command: ["session_list.select_tab", { tab: "Preview" }] });
});

test("the proof line lists every agent state, zero included", () => {
  // `scripts/proof/d2-board.sh` waits for "waiting is 0": a state with no
  // cards must still be on the line, or that wait can never succeed.
  assert.deepEqual(agentStateCounts([]), [
    ["working", 0],
    ["waiting", 0],
    ["idle", 0],
    ["unverifiable", 0],
    ["exited", 0],
  ]);
  const counted = agentStateCounts([
    card("a:1", { state: "working" }),
    card("a:2", { state: "working" }),
    card("a:3", { state: "idle" }),
    { state: "paused" } as unknown as AgentCardFrame,
  ]);
  assert.deepEqual(counted, [
    ["working", 2],
    ["waiting", 0],
    ["idle", 1],
    ["unverifiable", 0],
    ["exited", 0],
  ], "a state this build does not know is left off, since the host could not parse it back");
});

test("a legacy card with no session row is titled by its tmux target", () => {
  const { sessions, fleet } = world(["u-1", "api", "p-1"]);
  const columns = boardColumns(status(card("legacy:claude:hangar-dev:1.0:4242-17", { state: "waiting" })), fleet, sessions, NO_ACKS);
  const titles = columns.flatMap((column) => column.cards.map((c) => c.title));
  assert.deepEqual(titles, ["hangar-dev:1.0"]);
});
