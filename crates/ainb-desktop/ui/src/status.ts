// The operator's status vocabulary (spec: "Status vocabulary (deviates from
// Orca)"): five words a person can act on, derived from the host's own
// `AgentCardFrame` and a session row's attention chips, never re-derived from
// a lifecycle string or guessed from a chip's kind alone.
//
//   AgentCardFrame.state, wait_kind, turn_complete  ──┐
//   session.attention[] (kind + free-text detail)     ├─▶ UiStatus
//   this viewer's acks (acks.ts)                    ──┘
//
// The same `UiStatus` feeds the tab chip, the sidebar row's dot and the
// board's card (P4): one mapping, drawn three ways, so a state that reads
// "waiting" cannot show as amber on the board and mint in the sidebar.

import type {
  AgentCardFrame,
  AttentionKind,
  AttentionMark_Serialize,
  SessionFleetMetadata,
  Session_Serialize,
  WaitKind,
} from "../../../ainb-app/bindings/AppState";
import type { AckMap } from "./acks.ts";
import { isAcked, rowAckKey } from "./acks.ts";
import { legacyTmuxSession, providerId, ringFor, rowStatus } from "./sessions.ts";
import type { TabTarget } from "./tabs.ts";

/** What a "needs you" card is blocked on. */
export type Need = "ask" | "approve" | "wait" | "elicitation" | "error";

/**
 * The vocabulary itself: the spec table's five words, plus `exited` for a
 * session whose process is gone. The board keeps an exited agent in Done
 * until it is opened; once acked it counts as idle, as Orca settles an
 * acknowledged finished agent into gray idle, and sits in Idle marked Exited
 * (`board.ts columnOf`). A sidebar row or a tab for one still reads
 * `exited`, never plain idle: nothing is running there.
 */
export type UiStatus =
  | { kind: "needs"; need: Need }
  | { kind: "working" }
  | { kind: "done" }
  | { kind: "idle" }
  | { kind: "unverifiable" }
  | { kind: "exited" };

/**
 * A value the type system says cannot happen, and what to show if it does.
 *
 * Compile time: the `never` parameter makes a switch that misses a variant
 * fail to type-check, so a wire variant added in Rust is handled here before
 * it ships. Run time: a host at another version can still send a value this
 * build has never seen, and throwing inside a render blanks the whole shell
 * (there is no error boundary). So it answers `fallback`, the most honest
 * reading of "something this build cannot name" at each call site.
 */
export function unhandled<T>(value: never, fallback: T): T {
  // Once per value: this runs on every render, and a host at another version
  // sends the same unknown word on every frame.
  const key = String(value);
  if (!warned.has(key)) {
    warned.add(key);
    console.warn(`unhandled status input from the host: ${key}`);
  }
  return fallback;
}

/** The unknown values `unhandled` has already warned about. */
const warned = new Set<string>();

/** The `AgentCardFrame` fields the mapping reads. A fixture only has to carry
 * these, not a whole roster row. */
export type CardStatusInput = Pick<
  AgentCardFrame,
  "state" | "wait_kind" | "turn_complete" | "has_open_request" | "tier"
>;

/** What `deriveStatus` needs beyond the card itself. */
export interface DeriveStatusOptions {
  /** The row's merged attention chips, tightest first: only an `Err` chip
   * changes anything here, and only when the card's own `state` did not
   * already call it `waiting`. */
  attention?: readonly AttentionKind[];
  /** Whether a "waiting" `wait_kind`'s own chip named the MCP elicitation
   * flow explicitly (`elicitationDetail`). Defaults to `false`: the wire's
   * `WaitKind` carries no elicitation variant of its own yet. */
  elicitation?: boolean;
  /** Whether this viewer already acked this card's current turn
   * (`acks.ts`, keyed by `evidence_observed_at`). Defaults to `false`. */
  acked?: boolean;
}

/** `wait_kind` onto the need it names; `"waiting"` reads as `"elicitation"`
 * only when the caller's own chip-detail scan (`elicitationDetail`) found
 * that word, and as `"wait"` otherwise, which is every case today. A card
 * that is `waiting` with no `wait_kind` still needs a person: `"wait"`. */
function needFromWaitKind(waitKind: WaitKind | null, elicitation: boolean): Need {
  if (waitKind === null) return "wait";
  switch (waitKind) {
    case "ask":
      return "ask";
    case "approval":
      return "approve";
    case "error":
      return "error";
    case "waiting":
      return elicitation ? "elicitation" : "wait";
    default:
      return unhandled(waitKind, elicitation ? "elicitation" : "wait");
  }
}

/**
 * Whether any of `attention`'s own detail text names the MCP elicitation
 * flow. Free text, not a wire enum, because the daemon has nothing more
 * structured to send yet (see `needFromWaitKind`); absent that word this
 * reads `false`, which is every row a real daemon sends today.
 */
export function elicitationDetail(attention: readonly Pick<AttentionMark_Serialize, "detail">[]): boolean {
  return attention.some((mark) => mark.detail !== null && /elicitation/i.test(mark.detail));
}

/**
 * The vocabulary for one card, from the host's own `state`/`wait_kind`/
 * `turn_complete` plus its row's attention chips. An exited agent reads
 * `exited`, which every surface marks.
 *
 * `has_open_request` and `tier` ride on `card` (the shape a caller
 * destructures straight off `AgentCardFrame`) but never change the kind
 * here. `has_open_request` only decides which needs-you card floats to the
 * top of its column, which is the board's own sort, not this mapping.
 * `tier` is informational: only `Tier::Hook` and `Tier::AcpFeed` may assert
 * `waiting` at all (`Tier`'s own doc comment), so by the time `state` reads
 * `waiting` here the daemon has already enforced that invariant.
 */
export function deriveStatus(card: CardStatusInput, opts: DeriveStatusOptions = {}): UiStatus {
  switch (card.state) {
    case "exited":
      return { kind: "exited" };
    case "waiting":
      return { kind: "needs", need: needFromWaitKind(card.wait_kind, opts.elicitation ?? false) };
    case "working":
    case "idle":
    case "unverifiable":
      break;
    default:
      return unhandled(card.state, { kind: "unverifiable" });
  }
  // An error chip (or an error wait_kind) outranks what the card was doing.
  if ((opts.attention ?? []).includes("Err") || card.wait_kind === "error") {
    return { kind: "needs", need: "error" };
  }
  switch (card.state) {
    case "working":
      return { kind: "working" };
    case "idle":
      return card.turn_complete && !(opts.acked ?? false) ? { kind: "done" } : { kind: "idle" };
    case "unverifiable":
      // We hold the session but nothing has told us its state, which is never
      // the same fact as "idle" (AgentState's own doc comment).
      return { kind: "unverifiable" };
  }
}

/**
 * Whether `card` is `session`'s own card: the ONE join the board, the sidebar
 * row and the tab all use, so the same agent reads the same status on each.
 *
 * - By provider id: `fleet_metadata[session.id].provider_session_id` against
 *   the provider id in `session_key`. No cwd fallback.
 * - Else, for a legacy card (`legacy:<provider>:<target>:<fingerprint>`,
 *   `SessionKey::legacy`), which carries no provider id: the tmux session its
 *   target names against the row's `tmux_session_name`.
 */
export function cardBelongsTo(
  card: AgentCardFrame,
  session: Session_Serialize,
  metadata: Readonly<Record<string, SessionFleetMetadata>> | undefined,
): boolean {
  const providerSessionId = metadata?.[session.id]?.provider_session_id;
  if (providerSessionId !== null && providerSessionId !== undefined && providerId(card.session_key) === providerSessionId) {
    return true;
  }
  const legacy = legacyTmuxSession(card.session_key);
  return legacy !== null && session.tmux_session_name === legacy;
}

/**
 * `session`'s own card by [`cardBelongsTo`], or `undefined` when none
 * matches. A card joined by provider id wins over a legacy one: it is the
 * exact identity, the tmux session only the place it was last seen. Shared by
 * `statusForSession` and by anything that needs the card itself (`main.tsx`
 * acks a tab's session on the same join when it opens it).
 */
export function cardForSession(
  session: Session_Serialize,
  cards: readonly AgentCardFrame[],
  metadata: Readonly<Record<string, SessionFleetMetadata>> | undefined,
): AgentCardFrame | undefined {
  const matches = cards.filter((card) => cardBelongsTo(card, session, metadata));
  return matches.find((card) => legacyTmuxSession(card.session_key) === null) ?? matches[0];
}

/** The session row `card` belongs to by [`cardBelongsTo`], or `undefined`:
 * the board's side of the same join. */
export function sessionForCard(
  card: AgentCardFrame,
  sessions: readonly Session_Serialize[],
  metadata: Readonly<Record<string, SessionFleetMetadata>> | undefined,
): Session_Serialize | undefined {
  return sessions.find((session) => cardBelongsTo(card, session, metadata));
}

/**
 * `deriveStatus` for `session`, joined to its card the way the board joins
 * one (`fleet_metadata[session.id].provider_session_id` against the provider
 * id in `session_key`, no cwd fallback): the same card must read the same
 * status on the sidebar row, the tab chip and the board card.
 *
 * Falls back to the row's own merged ring (`ringFor`) and lifecycle
 * (`rowStatus`) when no card matches: an older host, a shell session no
 * card was ever built for, or a read this window's Fleet frame has not
 * caught up with yet. Lower confidence than a card (no `wait_kind`, no
 * evidence tier), but still five words rather than nothing.
 */
export function statusForSession(
  session: Session_Serialize,
  cards: readonly AgentCardFrame[],
  metadata: Readonly<Record<string, SessionFleetMetadata>> | undefined,
  acks: AckMap,
): UiStatus {
  const card = cardForSession(session, cards, metadata);
  if (card !== undefined) {
    const attention = (session.attention ?? []).map((mark) => mark.kind);
    return deriveStatus(card, {
      attention,
      elicitation: elicitationDetail(session.attention ?? []),
      acked: isAcked(acks, card.session_key, card.evidence_observed_at),
    });
  }
  const ring = ringFor(session);
  if (ring !== null) {
    switch (ring) {
      case "Ask":
        return { kind: "needs", need: "ask" };
      case "Wait":
        return { kind: "needs", need: "wait" };
      case "Approve":
        return { kind: "needs", need: "approve" };
      case "Err":
        return { kind: "needs", need: "error" };
      case "Done":
        // A chip-only Done has no card turn to ack; it is acked by row, and
        // the ack is pruned once the chip clears (`main.tsx`), so the next
        // Done on this row shows again.
        return isAcked(acks, rowAckKey(session.id), 0) ? { kind: "idle" } : { kind: "done" };
      default:
        return unhandled(ring, { kind: "unverifiable" });
    }
  }
  const lifecycle = rowStatus(session.status);
  switch (lifecycle) {
    // "Running" is the tmux session being alive, not the agent doing work: a
    // shell tab and an agent waiting at its prompt both read it. With no card
    // and no chip, nothing has said what the agent is doing, which is
    // exactly `unverifiable` (never "working", which would spin for every
    // idle shell).
    case "running":
      return { kind: "unverifiable" };
    case "error":
      return { kind: "needs", need: "error" };
    case "idle":
      return { kind: "idle" };
    // Stopped: the session's tmux is gone. Nothing runs, which is `exited`,
    // not a resting `idle`.
    case "stopped":
      return { kind: "exited" };
    // A status this build does not know: nothing it can name, so the honest
    // reading is unverifiable, never exited (which would hide a live agent).
    case "unknown":
      return { kind: "unverifiable" };
    default:
      return unhandled(lifecycle, { kind: "unverifiable" });
  }
}

/**
 * `statusForSession` for the session a terminal tab's target names, or
 * `null` for a tab with no session (a bare tmux pane) or one this window's
 * Sessions frame has not listed. Shared by the tab strip and the sidebar so
 * a tab and its row never disagree about the glyph before their title.
 */
export function statusForTarget(
  target: TabTarget,
  sessions: readonly Session_Serialize[],
  cards: readonly AgentCardFrame[],
  metadata: Readonly<Record<string, SessionFleetMetadata>> | undefined,
  acks: AckMap,
): UiStatus | null {
  if (target.kind !== "session") return null;
  const session = sessions.find((row) => row.id === target.id);
  return session === undefined ? null : statusForSession(session, cards, metadata, acks);
}

/**
 * The key a status paints with: a `data-status` attribute value shared by
 * the tab chip, the sidebar row's dot and the board card, so one CSS rule
 * per key draws the glyph and colour everywhere it appears.
 */
export function statusKey(status: UiStatus): string {
  return status.kind === "needs" ? `needs-${status.need}` : status.kind;
}

/** The operator-facing label for `status`, in the spec table's own words. */
export function statusLabel(status: UiStatus): string {
  switch (status.kind) {
    case "needs":
      return `Needs you · ${status.need}`;
    case "working":
      return "Working";
    case "done":
      return "Done";
    case "idle":
      return "Idle";
    case "unverifiable":
      return "Unverifiable";
    case "exited":
      return "Exited";
    default:
      return unhandled(status, "Unverifiable");
  }
}
