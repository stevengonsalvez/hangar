// The board's cards and its waiting list keep their DOM nodes across frames
// that do not change them (#1267), the same rule the palette follows.
//
// Mounted for real into a happy-dom document (see `window.ts`): the nodes the
// test holds are the nodes a pointer would be over.

import { settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent, createSignal } from "solid-js";
import { render } from "solid-js/web";
import type {
  AgentStatusView,
  FleetView_Serialize,
  SessionsView_Serialize,
} from "../../../ainb-app/bindings/AppState";
import { ackTurn, NO_ACKS, type AckMap } from "../../src/acks.ts";
import { Board } from "../../src/board.tsx";
import type { RendererIntent } from "../../src/tabs.ts";

/** One session row, with the provider id the fleet metadata correlates. */
function session(id: string, name = id) {
  return { id, name, status: "Running", branch_name: `agents/${id}`, workspace_path: "/repo", attention: [] };
}

/** The three frames the board draws from, new objects on every call. */
function frames(names: { a: string; b: string }) {
  const sessions = {
    workspaces: [{ name: "repo", sessions: [session("a", names.a), session("b", names.b)] }],
    selected_session_id: null,
  } as unknown as SessionsView_Serialize;
  const fleet = {
    fleet_metadata: { a: { provider_session_id: "pa" }, b: { provider_session_id: "pb" } },
    fleet_snapshot: [],
    daemon_attention: { by_session_id: { pa: [{ kind: "Ask", detail: "pick one" }] } },
    daemon_reachable: true,
  } as unknown as FleetView_Serialize;
  const agentStatus = {
    absent: null,
    view: {
      health: { kind: "fresh" },
      cards: [
        {
          session_key: "claude:pa",
          state: "waiting",
          provider: "claude",
          lifecycle: "running",
          transport_health: "ok",
          wait_kind: "ask",
          has_open_request: true,
          turn_complete: false,
          tier: "hook",
          evidence_observed_at: 1,
        },
        {
          session_key: "claude:pb",
          state: "working",
          provider: "claude",
          lifecycle: "running",
          transport_health: "ok",
          wait_kind: null,
          has_open_request: false,
          turn_complete: false,
          tier: "hook",
          evidence_observed_at: 1,
        },
      ],
    },
  } as unknown as AgentStatusView;
  return { sessions, fleet, agentStatus };
}

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  document.body.innerHTML = "";
});

/** The board, open over one frame set, with every chosen intent recorded. */
async function open(initial = frames({ a: "a", b: "b" })) {
  const [view, setView] = createSignal(initial);
  const [acks, setAcks] = createSignal<AckMap>(NO_ACKS);
  const chosen: RendererIntent[] = [];
  const transcripts: string[] = [];
  const acked: [string, number][] = [];
  const container = document.createElement("div");
  document.body.appendChild(container);
  cleanup = render(
    () =>
      createComponent(Board, {
        get agentStatus() {
          return view().agentStatus;
        },
        get fleet() {
          return view().fleet;
        },
        get sessions() {
          return view().sessions;
        },
        get acks() {
          return acks();
        },
        elsewhere: 0,
        onChoose: (intent: RendererIntent) => chosen.push(intent),
        onOpenTranscript: (key: string) => transcripts.push(key),
        onAck: (sessionKey: string, turnMarker: number) => {
          acked.push([sessionKey, turnMarker]);
          setAcks((current) => ackTurn(current, sessionKey, turnMarker));
        },
      }),
    container,
  );
  await settle();
  return {
    setView,
    setAcks,
    chosen,
    transcripts,
    acked,
    container,
    cards: () => [...container.querySelectorAll<HTMLButtonElement>(".board-card")],
    card: (key: string) => container.querySelector<HTMLButtonElement>(`.board-card[data-card="${key}"]`),
    column: (state: string) => container.querySelector<HTMLElement>(`.board-column[data-state="${state}"]`),
    rows: () => [...container.querySelectorAll<HTMLButtonElement>(".attention-row")],
  };
}

test("a frame that changes nothing keeps every card and waiting row", async () => {
  const board = await open();
  const cards = board.cards();
  const rows = board.rows();
  assert.deepEqual(
    cards.map((node) => node.dataset.card),
    ["claude:pa", "claude:pb"],
  );
  assert.equal(rows.length, 1, "one row is waiting");

  board.setView(frames({ a: "a", b: "b" }));
  await settle();

  // Compared with ===, not assert.equal: a failing assert.equal inspects both
  // nodes, and a happy-dom node's object graph takes minutes to print.
  board.cards().forEach((node, index) => assert.ok(node === cards[index], `card ${node.dataset.card} was rebuilt`));
  board.rows().forEach((node, index) => assert.ok(node === rows[index], `row ${node.dataset.row} was rebuilt`));
});

test("a card clicked after 50 frames still shows that card's session", async () => {
  const board = await open();
  // Held the way a pointer holds it: found once, clicked later.
  const card = board.card("claude:pb");
  assert.ok(card);

  // Each frame changes the OTHER card, as a live board does: this card is
  // unchanged, and a list rebuilt from new objects would still replace it.
  for (let n = 0; n < 50; n += 1) {
    board.setView(frames({ a: `a${n}`, b: "b" }));
    await settle();
  }

  assert.ok(card.isConnected, "the card is still the one on screen");
  card.click();
  assert.ok(board.chosen.length > 0, "the click dispatched nothing");
  assert.equal(
    JSON.stringify(board.chosen).includes('"b"'),
    true,
    `the click named another session: ${JSON.stringify(board.chosen)}`,
  );
});

test("a waiting row clicked after 50 frames still names its session", async () => {
  const board = await open();
  const row = board.rows()[0];
  assert.ok(row);

  for (let n = 0; n < 50; n += 1) {
    board.setView(frames({ a: "a", b: `b${n}` }));
    await settle();
  }

  assert.ok(row.isConnected, "the waiting row is still the one on screen");
  row.click();
  assert.ok(board.chosen.length > 0, "the click dispatched nothing");
  assert.ok(JSON.stringify(board.chosen).includes('"a"'), JSON.stringify(board.chosen));
});

test("a card whose title changes keeps its node and shows the new title", async () => {
  const board = await open();
  const card = board.card("claude:pb");
  assert.ok(card);

  board.setView(frames({ a: "a", b: "renamed" }));
  await settle();

  assert.ok(board.card("claude:pb") === card, "the renamed card is the same node");
  assert.equal(card.querySelector(".card-title")?.textContent, "renamed");
});

test("the columns are Orca's own words, and a needs-you card carries its need", async () => {
  const board = await open();
  assert.deepEqual(
    [...board.container.querySelectorAll(".board-column")].map((node) => node.getAttribute("data-state")),
    ["needs", "working", "done", "idle"],
  );
  const needsCard = board.card("claude:pa")!;
  assert.equal(needsCard.dataset.status, "needs-ask");
  assert.equal(needsCard.querySelector(".card-need")?.textContent, "ask");
  const workingCard = board.card("claude:pb")!;
  assert.equal(workingCard.dataset.status, "working");
  assert.equal(workingCard.querySelector(".card-need"), null, "only a needs-you card carries the chip");
});

/** A world with one idle, one unverifiable and one done card, for the idle
 * toggle, the unverifiable badge and the Done ack. */
function idleWorld() {
  const sessions = {
    workspaces: [
      {
        name: "repo",
        sessions: [
          session("idle-row"),
          session("unv-row"),
          session("done-row"),
          session("gone-row"),
          session("wait-row"),
          session("work-row"),
        ],
      },
    ],
    selected_session_id: null,
  } as unknown as SessionsView_Serialize;
  const fleet = {
    fleet_metadata: {
      "idle-row": { provider_session_id: "p-idle" },
      "unv-row": { provider_session_id: "p-unv" },
      "done-row": { provider_session_id: "p-done" },
      "gone-row": { provider_session_id: "p-gone" },
      "wait-row": { provider_session_id: "p-wait" },
      "work-row": { provider_session_id: "p-work" },
    },
    fleet_snapshot: [],
    daemon_attention: { by_session_id: {} },
    daemon_reachable: true,
  } as unknown as FleetView_Serialize;
  const agentStatus = {
    absent: null,
    view: {
      health: { kind: "fresh" },
      cards: [
        {
          session_key: "claude:p-idle",
          state: "idle",
          provider: "claude",
          lifecycle: "running",
          transport_health: "ok",
          wait_kind: null,
          has_open_request: false,
          turn_complete: false,
          tier: "hook",
          evidence_observed_at: 1,
        },
        {
          session_key: "claude:p-unv",
          state: "unverifiable",
          provider: "claude",
          lifecycle: "running",
          transport_health: "ok",
          wait_kind: null,
          has_open_request: false,
          turn_complete: false,
          tier: "pane_text",
          evidence_observed_at: 1,
        },
        {
          session_key: "claude:p-done",
          state: "idle",
          provider: "claude",
          lifecycle: "running",
          transport_health: "ok",
          wait_kind: null,
          has_open_request: false,
          turn_complete: true,
          tier: "hook",
          evidence_observed_at: 42,
        },
        {
          session_key: "claude:p-gone",
          state: "exited",
          provider: "claude",
          lifecycle: "exited",
          transport_health: "unavailable",
          wait_kind: null,
          has_open_request: false,
          turn_complete: false,
          tier: "hook",
          evidence_observed_at: 7,
        },
        {
          session_key: "claude:p-wait",
          state: "waiting",
          provider: "claude",
          lifecycle: "running",
          transport_health: "ok",
          wait_kind: "ask",
          has_open_request: true,
          turn_complete: false,
          tier: "hook",
          evidence_observed_at: 3,
        },
        {
          session_key: "claude:p-work",
          state: "working",
          provider: "claude",
          lifecycle: "running",
          transport_health: "ok",
          wait_kind: null,
          has_open_request: false,
          turn_complete: false,
          tier: "hook",
          evidence_observed_at: 3,
        },
        {
          session_key: "claude:p-orphan",
          state: "exited",
          provider: "claude",
          lifecycle: "exited",
          transport_health: "ok",
          wait_kind: null,
          has_open_request: false,
          turn_complete: false,
          tier: "hook",
          evidence_observed_at: 3,
        },
      ],
    },
  } as unknown as AgentStatusView;
  return { sessions, fleet, agentStatus };
}

test("every column draws exactly the cards its count says, Idle and exited agents included", async () => {
  const board = await open(idleWorld());
  assert.equal(board.container.querySelector("details"), null, "no column hides its cards behind a disclosure");
  const drawn: Record<string, string[]> = {};
  for (const column of board.container.querySelectorAll<HTMLElement>(".board-column")) {
    const cards = [...column.querySelectorAll<HTMLElement>(".board-card")].map((card) => card.dataset.card!);
    const count = Number(column.querySelector(".board-count")?.textContent);
    assert.equal(count, cards.length, `the ${column.dataset.state} column's count is its own cards`);
    drawn[column.dataset.state!] = cards;
  }
  assert.deepEqual(drawn.needs, ["claude:p-wait"]);
  assert.deepEqual(drawn.working, ["claude:p-work"]);
  assert.deepEqual(drawn.done, ["claude:p-done", "claude:p-gone"], "an exited agent is kept, in Done");
  assert.deepEqual(drawn.idle, ["claude:p-orphan", "claude:p-idle", "claude:p-unv"], "the row-less exited agent too");
  const unverifiable = board.card("claude:p-unv")!;
  assert.equal(unverifiable.dataset.status, "unverifiable");
  assert.ok(unverifiable.querySelector(".badge-unverifiable"), "the card carries the unverifiable badge");
});

test("an exited agent reads Exited in Done, and once opened settles into Idle", async () => {
  const board = await open(idleWorld());
  const gone = board.card("claude:p-gone")!;
  assert.equal(gone.dataset.status, "exited");
  assert.equal(gone.querySelector(".badge-exited")?.textContent, "Exited");
  assert.ok(board.column("done")!.contains(gone));

  gone.click();
  assert.deepEqual(board.acked, [["claude:p-gone", 7]]);
  await settle();
  const after = board.card("claude:p-gone")!;
  assert.ok(board.column("idle")!.contains(after), "opened: it settles into Idle, still drawn");
  assert.ok(after.querySelector(".badge-exited"), "and still says it exited");
});

test("an exited agent with no row waits in Idle, disabled, and never says click to open", async () => {
  const board = await open(idleWorld());
  const orphan = board.card("claude:p-orphan")! as HTMLButtonElement;
  assert.ok(board.column("idle")!.contains(orphan));
  assert.equal(orphan.disabled, true);
  assert.notEqual(orphan.querySelector(".card-line")?.textContent, "click to open");
  assert.ok(orphan.querySelector(".badge-exited"));
});

test("a Done card says click to open, and clicking it acks the turn and moves it to Idle", async () => {
  const board = await open(idleWorld());
  const done = board.card("claude:p-done")!;
  assert.equal(done.dataset.status, "done");
  assert.equal(done.querySelector(".card-line")?.textContent, "click to open");

  done.click();
  assert.deepEqual(board.acked, [["claude:p-done", 42]]);
  await settle();

  const afterAck = board.card("claude:p-done");
  assert.ok(afterAck, "the same agent's card still draws");
  assert.equal(afterAck.dataset.status, "idle", "acked: no longer Done");
  assert.ok(board.column("idle")!.contains(afterAck), "and it moved into the Idle column");
});
