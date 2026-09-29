// An exited agent across a relaunch of the window. The host rebuilds a
// stopped row for it from `sessions.json`, carrying the id it was launched
// under, so the Fleet metadata joins the row to its card again: the card keeps
// the row's name. Relaunching is never an ack. This viewer's acks are kept in
// storage, as Orca persists `acknowledgedAgentsByPaneKey`, so an exit acked
// before quitting stays in Idle, and one that ended while the window was shut
// reads Done until it is opened.

import assert from "node:assert/strict";
import { test } from "node:test";
import type {
  AgentCardFrame,
  AgentStatusView,
  FleetView_Serialize,
  SessionsView_Serialize,
} from "../../../ainb-app/bindings/AppState";
import { ackTurn, NO_ACKS, readAcks, writeAcks, type AckStorage } from "./acks.ts";
import { boardColumns, countIn } from "./board.ts";

function exited(sessionKey: string, evidence: number): AgentCardFrame {
  return {
    session_key: sessionKey,
    provider: "claude",
    lifecycle: "EXITED",
    transport_health: "HEALTHY",
    state: "exited",
    has_open_request: false,
    wait_kind: null,
    turn_complete: false,
    tier: "hook",
    evidence_observed_at: evidence,
  } as AgentCardFrame;
}

function status(...cards: AgentCardFrame[]): AgentStatusView {
  return {
    absent: null,
    head_revision: 3,
    view: { host_id: "local", read_revision: 3, received_at_ms: 1, head_revision: 3, health: { kind: "live" }, cards },
  };
}

/** The frames after a relaunch: one Stopped row per `[id, name, provider id]`,
 * joined to its card by the provider id the host now restores on it. */
function relaunched(...rows: [string, string, string][]) {
  const sessions = {
    workspaces: [
      {
        name: "repo",
        sessions: rows.map(([id, name]) => ({
          id,
          name,
          // The host reads the branch back off the worktree's checkout when it
          // rebuilds a stopped row (`stopped_session_from_metadata`).
          branch_name: `agents/${name}`,
          status: "Stopped",
          attention: [],
        })),
      },
    ],
  } as unknown as SessionsView_Serialize;
  const fleet = {
    fleet_metadata: Object.fromEntries(rows.map(([id, , provider]) => [id, { provider_session_id: provider }])),
    fleet_snapshot: [],
    daemon_attention: { by_session_id: {}, all: {}, reachable: true, error: null, not_running: false },
    attention_elsewhere: 0,
  } as unknown as FleetView_Serialize;
  return { sessions, fleet };
}

function memory(): AckStorage {
  const data = new Map<string, string>();
  return { getItem: (key) => data.get(key) ?? null, setItem: (key, value) => void data.set(key, value) };
}

const ACKED = "60d6bea2-acked";
const UNSEEN = "7c1f09e4-unseen";

test("after a relaunch an exited card keeps its row's name, never the key fallback", () => {
  const { sessions, fleet } = relaunched(["u-1", "fix-login", ACKED]);
  const cards = boardColumns(status(exited(`claude:${ACKED}`, 5)), fleet, sessions, NO_ACKS).flatMap((c) => c.cards);
  assert.equal(cards.length, 1);
  assert.equal(cards[0].title, "fix-login");
  assert.equal(cards[0].sessionId, "u-1", "the card opens its row");
});

test("after a relaunch an exited card keeps its row's branch", () => {
  const { sessions, fleet } = relaunched(["u-1", "claude", ACKED]);
  const cards = boardColumns(status(exited(`claude:${ACKED}`, 5)), fleet, sessions, NO_ACKS).flatMap((c) => c.cards);
  assert.equal(cards[0].branch, "agents/claude");
});

test("a relaunch is not an ack: a stored ack keeps its exit in Idle, an unseen exit stays Done", () => {
  // Before quitting, this viewer opened the first agent's exit.
  const storage = memory();
  writeAcks(storage, ackTurn(NO_ACKS, `claude:${ACKED}`, 5));

  // Relaunched: the acks come back from storage; the second agent ended while
  // the window was shut, so nobody has seen it.
  const { sessions, fleet } = relaunched(["u-1", "fix-login", ACKED], ["u-2", "add-search", UNSEEN]);
  const columns = boardColumns(
    status(exited(`claude:${ACKED}`, 5), exited(`claude:${UNSEEN}`, 9)),
    fleet,
    sessions,
    readAcks(storage),
  );
  const titles = Object.fromEntries(columns.map((column) => [column.state, column.cards.map((card) => card.title)]));
  assert.deepEqual(titles, { needs: [], working: [], done: ["add-search"], idle: ["fix-login"] });
  assert.equal(countIn(columns, "idle"), 1, "the footer's idle counts the acked exit only");
  assert.equal(countIn(columns, "done"), 1);
});
