// What the board and the attention list draw, projected from the frames the
// window already holds. Projections only: `board.tsx` draws what these
// return, and nothing here reads the store or keeps anything.
//
//   agent_status.view.cards[] ──status.ts──▶ Orca's four buckets
//   fleet.fleet_metadata[]    ──model, session_key──▶ the card's line
//   sessions.workspaces[]     ──name, attention──▶ the card's title and chips
//   acks.ts                  ──this viewer's Done acks──▶ Done vs Idle
//
// The bucket is `status.ts`'s own mapping of the host's `state`, never
// re-derived here from a lifecycle or an attention chip: two surfaces
// guessing differently from the same frame is the drift this seam exists to
// remove (P4 carries that seam from the raw `AgentState` onto the operator's
// own four words).

import type {
  AgentState,
  AgentStatusView,
  AttentionKind,
  FleetView_Serialize,
  SessionAttention_Serialize,
  SessionsView_Serialize,
  Session_Serialize,
} from "../../../ainb-app/bindings/AppState";
import type { AckMap } from "./acks.ts";
import { isAcked } from "./acks.ts";
import { allSessions, ATTENTION_ORDER, keyLabel, label, providerId } from "./sessions.ts";
import { deriveStatus, elicitationDetail, unhandled, type UiStatus } from "./status.ts";
import type { RendererIntent } from "./tabs.ts";

/** Orca's own board buckets, left to right. Idle also holds a card whose
 * status is `unverifiable`: the vocabulary has no fifth column for it, so it
 * falls in with idle and carries its own badge (`BoardCard.unverifiable`). */
export type BoardColumnKind = "needs" | "working" | "done" | "idle";

/** What each column is called, in the operator's own words. */
export const COLUMN_TITLES: Record<BoardColumnKind, string> = {
  needs: "Needs you",
  working: "Working",
  done: "Done",
  idle: "Idle",
};

/** The columns the board draws, left to right. */
export const COLUMNS: BoardColumnKind[] = ["needs", "working", "done", "idle"];

/** One card on the board. */
export interface BoardCard {
  /** `provider:session-id`, the host's own identity for the agent. */
  key: string;
  /** The sidebar's name for the row, when the window holds one. */
  title: string;
  /** The session list row a click selects, when this card has one. */
  sessionId: string | null;
  /** The operator vocabulary this card reads (`status.ts`). */
  status: UiStatus;
  /** This card's own turn marker, for acking it (`acks.ts`). */
  evidenceObservedAt: number;
  /** The line under the title: provider, model, lifecycle, transport. */
  provider: string;
  model: string | null;
  lifecycle: string;
  transport: string;
  /** What the agent is waiting for, when it is waiting. */
  waitKind: string | null;
  /** Something is open on this agent, so it floats to the top of its column. */
  hasOpenRequest: boolean;
  /**
   * An ACP session: no tmux pane and no session list row, so its card opens
   * its transcript instead of a row.
   */
  acp: boolean;
  /** The chips its session row carries, tightest first. */
  attention: AttentionKind[];
}

/** One column: the bucket it holds and the cards in it. */
export interface BoardColumn {
  state: BoardColumnKind;
  cards: BoardCard[];
}

/**
 * What the board says about itself when it cannot draw agents: the section is
 * absent, the read is behind the daemon's head, or the daemon is unreachable.
 *
 * A board with no cards and no explanation reads as "nothing is running",
 * which is the one thing it must never say when it simply does not know.
 */
export type BoardHealth =
  | { kind: "live" }
  | { kind: "absent"; detail: string }
  | { kind: "stale"; behind: number }
  | { kind: "unreachable"; reason: string };

/**
 * Each session row by the provider session id the host correlated it with,
 * built once per projection rather than searched once per card.
 */
function rowsByProvider(
  sessions: SessionsView_Serialize | undefined,
  fleet: FleetView_Serialize | undefined,
): Map<string, Session_Serialize> {
  const metadata = fleet?.fleet_metadata ?? {};
  const rows = new Map<string, Session_Serialize>();
  for (const session of allSessions(sessions)) {
    const provider = metadata[session.id]?.provider_session_id;
    if (provider !== null && provider !== undefined) rows.set(provider, session);
  }
  return rows;
}

/** The chips a session row carries, tightest first. */
function chipsOf(session: Session_Serialize | undefined): AttentionKind[] {
  return [...(session?.attention ?? [])]
    .map((mark) => mark.kind)
    .sort((a, b) => ATTENTION_ORDER.indexOf(a) - ATTENTION_ORDER.indexOf(b));
}

/** `status.kind`, onto the column it draws in, or `null` for one the board
 * leaves out. `unverifiable` has no column of its own in Orca's four-bucket
 * vocabulary, so it falls in with `idle` (`BoardCard.unverifiable` carries
 * the badge that tells the two apart); `exited` is not a status anyone acts
 * on. Exhaustive: a new status fails to compile here. */
function bucketOf(status: UiStatus): BoardColumnKind | null {
  switch (status.kind) {
    case "needs":
      return "needs";
    case "working":
      return "working";
    case "done":
      return "done";
    case "idle":
    case "unverifiable":
      return "idle";
    case "exited":
      return null;
    default:
      return unhandled(status, "idle");
  }
}

/**
 * The board's columns, in `COLUMNS` order, from the host's own cards mapped
 * through `status.ts`. An exited agent's card never appears (`deriveStatus`
 * returns `null` for it): a process that is gone is not a status a person
 * acts on. Within a column, an agent with something open floats to the top,
 * then by title, so the row that wants a human is the first one read.
 */
export function boardColumns(
  agentStatus: AgentStatusView | undefined,
  fleet: FleetView_Serialize | undefined,
  sessions: SessionsView_Serialize | undefined,
  acks: AckMap,
): BoardColumn[] {
  const models = new Map((fleet?.fleet_snapshot ?? []).map((row) => [row.session_key, row.model]));
  const rows = rowsByProvider(sessions, fleet);
  const cards: BoardCard[] = [];
  for (const card of agentStatus?.view?.cards ?? []) {
    const session = rows.get(providerId(card.session_key));
    const attention = chipsOf(session);
    const status = deriveStatus(card, {
      attention,
      elicitation: elicitationDetail(session?.attention ?? []),
      acked: isAcked(acks, card.session_key, card.evidence_observed_at),
    });
    if (bucketOf(status) === null) continue; // exited: not a status anyone acts on.
    cards.push({
      key: card.session_key,
      title: label(session?.name ?? keyLabel(card.session_key)),
      sessionId: session?.id ?? null,
      status,
      evidenceObservedAt: card.evidence_observed_at,
      provider: card.provider,
      model: models.get(card.session_key) ?? null,
      lifecycle: card.lifecycle,
      transport: card.transport_health,
      waitKind: card.wait_kind,
      hasOpenRequest: card.has_open_request,
      acp: card.provider === "acp",
      attention,
    });
  }
  return COLUMNS.map((state) => ({
    state,
    cards: cards
      .filter((card) => bucketOf(card.status) === state)
      .sort(
        (a, b) =>
          Number(b.hasOpenRequest) - Number(a.hasOpenRequest) || a.title.localeCompare(b.title),
      ),
  }));
}

/** What the board can say about the picture it is drawing. */
export function boardHealth(agentStatus: AgentStatusView | undefined): BoardHealth {
  if (agentStatus === undefined) return { kind: "absent", detail: "no status frame yet" };
  if (agentStatus.absent !== null) return { kind: "absent", detail: label(agentStatus.absent) };
  const view = agentStatus.view;
  if (view === null) return { kind: "absent", detail: "no status read yet" };
  switch (view.health.kind) {
    case "stale":
      return { kind: "stale", behind: view.health.head_revision - view.health.read_revision };
    case "unreachable":
      return { kind: "unreachable", reason: label(view.health.reason) };
    default:
      return { kind: "live" };
  }
}

/** One row of the attention list: an agent waiting on a human. */
export interface AttentionRow {
  /** Unique within the list, so it keys the rendered rows. */
  key: string;
  title: string;
  kind: AttentionKind;
  detail: string | null;
  /** The session list row a click selects, when the window holds one. */
  sessionId: string | null;
}

/**
 * The attention list the header's counts summarise: the daemon's open rows,
 * keyed by the provider session id they were raised against.
 *
 * The daemon's rows, not the sidebar's chips, because a row whose session this
 * window has not listed still needs a human. The title falls back to the
 * provider id for exactly that case.
 */
export function attentionRows(
  fleet: FleetView_Serialize | undefined,
  sessions: SessionsView_Serialize | undefined,
): AttentionRow[] {
  const byProvider = rowsByProvider(sessions, fleet);
  const rows: AttentionRow[] = [];
  for (const [providerSessionId, chips] of Object.entries(fleet?.daemon_attention?.by_session_id ?? {})) {
    const session = byProvider.get(providerSessionId);
    (chips as SessionAttention_Serialize[]).forEach((chip, index) => {
      rows.push({
        key: `${providerSessionId}:${index}`,
        title: label(session?.name ?? providerSessionId),
        kind: chip.kind,
        detail: chip.detail === null ? null : label(chip.detail),
        sessionId: session?.id ?? null,
      });
    });
  }
  return rows.sort(
    (a, b) => ATTENTION_ORDER.indexOf(a.kind) - ATTENTION_ORDER.indexOf(b.kind) || a.title.localeCompare(b.title),
  );
}

/**
 * How many open rows belong to no session this window can show, as the host
 * counted them.
 *
 * Its own line rather than folded into the list: a count of things happening
 * somewhere else is not a row anyone can act on here, and dropping it would
 * quietly shrink the fleet to what this window happens to hold.
 */
export function elsewhereCount(fleet: FleetView_Serialize | undefined): number {
  return fleet?.attention_elsewhere ?? 0;
}

/** Whether the daemon answered the last attention poll. */
export function daemonReachable(fleet: FleetView_Serialize | undefined): boolean {
  return fleet?.daemon_attention?.reachable ?? false;
}

/**
 * What a click on a card or an attention row sends: its session list row
 * selected WITHOUT attaching it (a terminal is a decision of its own), then the
 * pane the click is about, which is `ask` when something is open on that agent.
 */
export function showIntents(sessionId: string, openRequest: boolean): RendererIntent[] {
  return [
    { Command: ["session_list.select_row", { target: { session: sessionId }, open: false }] },
    { Command: ["session_list.select_tab", { tab: openRequest ? "Ask" : "Preview" }] },
  ];
}

/** Every `AgentState`, in the order the proof line lists them. A `Record`, so
 * a state added in Rust fails the type check here until it is listed. */
const AGENT_STATE_ORDER: Record<AgentState, number> = {
  working: 0,
  waiting: 1,
  idle: 2,
  unverifiable: 3,
  exited: 4,
};

/**
 * How many cards the agent status frame carries per raw `AgentState`, for
 * the `renderer_applied` proof line only. Deliberately NOT the board's own
 * bucket counts: that Tauri command's `board` argument is typed
 * `Vec<(AgentState, usize)>` on the Rust side, and the board draws by the
 * operator's four words (needs/working/done/idle), which that enum cannot
 * parse. The wire contract stays what it was; the UI's bucketing is
 * `boardColumns`.
 *
 * Every state is listed, zero included: a proof that waits for "waiting is
 * 0" must read a 0, not a state missing from the line. A state this build
 * does not know is left out, since the host could not parse it back.
 */
export function agentStateCounts(cards: readonly { state: AgentState }[]): [AgentState, number][] {
  const states = Object.keys(AGENT_STATE_ORDER) as AgentState[];
  const counts = new Map<AgentState, number>(states.map((state) => [state, 0]));
  for (const card of cards) {
    if (counts.has(card.state)) counts.set(card.state, (counts.get(card.state) ?? 0) + 1);
  }
  return states.map((state) => [state, counts.get(state) ?? 0]);
}
