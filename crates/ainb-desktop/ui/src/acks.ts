// Done-until-ack, per viewer (spec: "Done stays until opened (ack, per
// viewer)"). A card reads "done" once and stays that way for every OTHER
// viewer's window; this viewer's own ack is a per-window convenience kept in
// localStorage, wrapped like `theme.ts`'s: storage can be missing or throw
// (a private window, blocked site data), and the board must still draw.
//
// Keyed by `session_key` plus the turn it acks (`evidence_observed_at`), not
// by `session_key` alone: acking the turn a Done card is showing must not
// silently ack a LATER turn that has not happened yet, or the next Done
// would never show.

/** The smallest storage surface this module needs, so tests can pass a fake
 * (`theme.ts`'s `ThemeStorage` and `sidebar_model.ts`'s `SidebarStorage` are
 * the same shape, for the same reason). */
export interface AckStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/** The localStorage key holding every session's last acked turn. */
export const ACKS_KEY = "ainb.board.acks";

/** `session_key` to the highest `evidence_observed_at` this viewer has
 * acked for it. */
export type AckMap = Readonly<Record<string, number>>;

/** No acks: every card reads whatever `deriveStatus` says on its own. */
export const NO_ACKS: AckMap = {};

/**
 * The stored acks, or `NO_ACKS` when storage is absent, throws, or holds
 * something that is not the shape this module wrote.
 */
export function readAcks(storage: AckStorage | undefined): AckMap {
  try {
    const raw = storage?.getItem(ACKS_KEY);
    if (!raw) return NO_ACKS;
    const parsed = JSON.parse(raw);
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return NO_ACKS;
    const acks: Record<string, number> = {};
    for (const [key, value] of Object.entries(parsed as Record<string, unknown>)) {
      if (typeof value === "number") acks[key] = value;
    }
    return acks;
  } catch {
    return NO_ACKS;
  }
}

/** Store `acks`; a storage that throws only loses the memory of it for this
 * window's life. */
export function writeAcks(storage: AckStorage | undefined, acks: AckMap): void {
  try {
    storage?.setItem(ACKS_KEY, JSON.stringify(acks));
  } catch {
    // Nothing to do: the acks still apply for this window's life.
  }
}

/**
 * Whether `turnMarker` (a card's `evidence_observed_at`) is already acked
 * for `sessionKey`: acked at exactly this turn, or a later one this viewer
 * somehow saw first. A turn with a GREATER marker than what is stored always
 * reads as not yet acked, which is what lets a later Done show again.
 */
export function isAcked(acks: AckMap, sessionKey: string, turnMarker: number): boolean {
  const acked = acks[sessionKey];
  return acked !== undefined && acked >= turnMarker;
}

/**
 * `acks` with `sessionKey` acked through `turnMarker`. Never moves an ack
 * backwards: acking a frame that arrived out of order after a newer turn
 * already showed must not un-ack the newer one.
 */
export function ackTurn(acks: AckMap, sessionKey: string, turnMarker: number): AckMap {
  if (isAcked(acks, sessionKey, turnMarker)) return acks;
  return { ...acks, [sessionKey]: turnMarker };
}

/**
 * The ack key for a row whose Done comes only from an attention chip (no
 * agent card, so no turn marker): acked at marker 0 until the chip clears.
 */
export function rowAckKey(sessionId: string): string {
  return `row:${sessionId}`;
}

/**
 * `acks` keeping only the keys in `live`: the cards and chip-only Done rows
 * this window can still see. Pruning is what lets a chip-only Done show
 * again after its chip clears and returns, and keeps storage from growing
 * with every session that ever existed. Returns `acks` itself when nothing
 * is dropped, so a caller can skip the write.
 */
export function pruneAcks(acks: AckMap, live: ReadonlySet<string>): AckMap {
  const kept = Object.entries(acks).filter(([key]) => live.has(key));
  if (kept.length === Object.keys(acks).length) return acks;
  return Object.fromEntries(kept);
}
