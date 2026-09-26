// The operator status vocabulary: `AgentState` x `WaitKind` x `turn_complete`
// mapped onto the five words a person can act on, plus the session-level
// fallback and the labels/keys every surface draws.

import assert from "node:assert/strict";
import { test } from "node:test";
import type {
  AgentCardFrame,
  AgentState,
  Session_Serialize,
  Tier,
  WaitKind,
} from "../../../ainb-app/bindings/AppState";
import { ackTurn, NO_ACKS } from "./acks.ts";
import {
  deriveStatus,
  elicitationDetail,
  statusForSession,
  statusForTarget,
  statusKey,
  statusLabel,
  type CardStatusInput,
  type UiStatus,
} from "./status.ts";

const STATES: AgentState[] = ["working", "waiting", "idle", "exited", "unverifiable"];
const WAIT_KINDS: (WaitKind | null)[] = ["ask", "approval", "waiting", "error", null];
const TIER: Tier = "hook";

function input(state: AgentState, waitKind: WaitKind | null, turnComplete: boolean): CardStatusInput {
  return { state, wait_kind: waitKind, turn_complete: turnComplete, has_open_request: false, tier: TIER };
}

/**
 * The expected status for every `(state, wait_kind)` pair, with `attention`
 * empty, `elicitation` false and `acked` false: every other test in this
 * file holds one of those three fixed and varies it on its own. `turn_complete`
 * only matters for `idle`, so the table below is keyed by state and wait_kind
 * alone, and the loop checks both `turn_complete` values against the same
 * expectation for every state except `idle`.
 */
const EXPECTED: Record<AgentState, UiStatus | null> = {
  working: { kind: "working" },
  waiting: { kind: "needs", need: "wait" }, // overridden per wait_kind below
  idle: { kind: "idle" }, // overridden per turn_complete below
  exited: null,
  unverifiable: { kind: "unverifiable" },
};

const WAITING_NEED: Record<WaitKind | "null", UiStatus> = {
  ask: { kind: "needs", need: "ask" },
  approval: { kind: "needs", need: "approve" },
  waiting: { kind: "needs", need: "wait" },
  error: { kind: "needs", need: "error" },
  null: { kind: "needs", need: "wait" },
};

test("every AgentState x WaitKind x turn_complete combination", () => {
  for (const state of STATES) {
    for (const waitKind of WAIT_KINDS) {
      for (const turnComplete of [true, false]) {
        const got = deriveStatus(input(state, waitKind, turnComplete));
        const key = waitKind ?? "null";
        // `wait_kind: "error"` needs a human on every state that reaches the
        // check (not just `waiting`, which already maps it through
        // `WAITING_NEED`): `exited` still hides the card outright.
        const expected =
          state === "exited"
            ? null
            : state === "waiting"
              ? WAITING_NEED[key]
              : waitKind === "error"
                ? { kind: "needs" as const, need: "error" as const }
                : state === "idle"
                  ? turnComplete
                    ? { kind: "done" as const }
                    : { kind: "idle" as const }
                  : EXPECTED[state];
        assert.deepEqual(
          got,
          expected,
          `state=${state} wait_kind=${String(waitKind)} turn_complete=${turnComplete}`,
        );
      }
    }
  }
});

test("exited hides the card whatever else the frame says", () => {
  assert.equal(deriveStatus(input("exited", "ask", true)), null);
  assert.equal(deriveStatus(input("exited", "error", false), { attention: ["Err"] }), null);
});

test("an Err attention chip needs a human even off a card the host has not called waiting", () => {
  assert.deepEqual(deriveStatus(input("working", null, false), { attention: ["Err"] }), {
    kind: "needs",
    need: "error",
  });
  assert.deepEqual(deriveStatus(input("idle", null, true), { attention: ["Err"] }), {
    kind: "needs",
    need: "error",
  });
  assert.deepEqual(deriveStatus(input("unverifiable", null, false), { attention: ["Err"] }), {
    kind: "needs",
    need: "error",
  });
});

test("a wait_kind of error needs a human even without an Err chip", () => {
  assert.deepEqual(deriveStatus(input("idle", "error", false)), { kind: "needs", need: "error" });
});

test("acking the current turn moves a finished idle card off Done", () => {
  const card = input("idle", null, true);
  assert.deepEqual(deriveStatus(card, { acked: false }), { kind: "done" });
  assert.deepEqual(deriveStatus(card, { acked: true }), { kind: "idle" });
});

test("a waiting card reads elicitation only when a chip's own detail names it", () => {
  const card = input("waiting", "waiting", false);
  assert.deepEqual(deriveStatus(card), { kind: "needs", need: "wait" }, "no elicitation flag: plain wait");
  assert.deepEqual(deriveStatus(card, { elicitation: true }), { kind: "needs", need: "elicitation" });
});

test("elicitationDetail reads a chip's own free-text detail, case-insensitively", () => {
  assert.equal(elicitationDetail([]), false);
  assert.equal(elicitationDetail([{ detail: null }]), false);
  assert.equal(elicitationDetail([{ detail: "pick a file" }]), false);
  assert.equal(elicitationDetail([{ detail: "waiting: ELICITATION requested" }]), true);
});

function session(id: string, over: Partial<Session_Serialize> = {}): Session_Serialize {
  return {
    id,
    name: id,
    workspace_path: "/repo",
    branch_name: `agents/${id}`,
    status: "Running",
    attention: [],
    ...over,
  } as Session_Serialize;
}

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
    evidence_observed_at: 1,
    ...over,
  } as AgentCardFrame;
}

test("statusForSession joins by provider id and defers to deriveStatus", () => {
  const row = session("u-1");
  const cards = [card("claude:p-1", { state: "waiting", wait_kind: "ask" })];
  const metadata = { "u-1": { provider_session_id: "p-1" } } as never;
  assert.deepEqual(statusForSession(row, cards, metadata, NO_ACKS), { kind: "needs", need: "ask" });
});

test("statusForSession falls back to the row's own ring and lifecycle with no matching card", () => {
  const asking = session("u-1", { attention: [{ kind: "Ask", detail: null, request: "r", options: [], route: "Pane" }] });
  assert.deepEqual(statusForSession(asking, [], {}, NO_ACKS), { kind: "needs", need: "ask" });

  const idle = session("u-2", { status: "Idle" });
  assert.deepEqual(statusForSession(idle, [], {}, NO_ACKS), { kind: "idle" });

  // A live tmux session with no card and no chip: alive, but nothing says
  // the agent is working, so it must not spin.
  const running = session("u-3", { status: "Running" });
  assert.deepEqual(statusForSession(running, [], {}, NO_ACKS), { kind: "unverifiable" });

  const errored = session("u-4", { status: { Error: "crashed" } as never });
  assert.deepEqual(statusForSession(errored, [], {}, NO_ACKS), { kind: "needs", need: "error" });
});

test("statusForSession honours this viewer's ack the same as the board does", () => {
  const row = session("u-1");
  const cards = [card("claude:p-1", { state: "idle", turn_complete: true, evidence_observed_at: 9 })];
  const metadata = { "u-1": { provider_session_id: "p-1" } } as never;
  assert.deepEqual(statusForSession(row, cards, metadata, NO_ACKS), { kind: "done" });
  assert.deepEqual(statusForSession(row, cards, metadata, ackTurn(NO_ACKS, "claude:p-1", 9)), { kind: "idle" });
});

test("statusForTarget is null for a bare tmux tab or a session this window has not listed", () => {
  assert.equal(statusForTarget({ kind: "tmux", tmux: "shell-1" }, [], [], {}, NO_ACKS), null);
  assert.equal(statusForTarget({ kind: "session", id: "gone", tmux: "t" }, [], [], {}, NO_ACKS), null);
});

test("statusForTarget resolves a session tab the same way statusForSession does", () => {
  const row = session("u-1");
  const cards = [card("claude:p-1", { state: "working" })];
  const metadata = { "u-1": { provider_session_id: "p-1" } } as never;
  assert.deepEqual(
    statusForTarget({ kind: "session", id: "u-1", tmux: "t" }, [row], cards, metadata, NO_ACKS),
    { kind: "working" },
  );
});

test("statusKey names one CSS hook per status, shared by the tab, the row and the card", () => {
  assert.equal(statusKey({ kind: "needs", need: "ask" }), "needs-ask");
  assert.equal(statusKey({ kind: "needs", need: "approve" }), "needs-approve");
  assert.equal(statusKey({ kind: "needs", need: "wait" }), "needs-wait");
  assert.equal(statusKey({ kind: "needs", need: "elicitation" }), "needs-elicitation");
  assert.equal(statusKey({ kind: "needs", need: "error" }), "needs-error");
  assert.equal(statusKey({ kind: "working" }), "working");
  assert.equal(statusKey({ kind: "done" }), "done");
  assert.equal(statusKey({ kind: "idle" }), "idle");
  assert.equal(statusKey({ kind: "unverifiable" }), "unverifiable");
});

test("statusLabel reads in the spec table's own words", () => {
  assert.equal(statusLabel({ kind: "needs", need: "approve" }), "Needs you · approve");
  assert.equal(statusLabel({ kind: "working" }), "Working");
  assert.equal(statusLabel({ kind: "done" }), "Done");
  assert.equal(statusLabel({ kind: "idle" }), "Idle");
  assert.equal(statusLabel({ kind: "unverifiable" }), "Unverifiable");
});
