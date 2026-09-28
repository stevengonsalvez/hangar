// What the sessions sidebar and the header draw from the Sessions frame.
// Projections only, in the manner of `wire::web::session_rows`:
// every function reads the frame bodies it is handed and keeps nothing, so a
// caller passing store proxies stays fine-grained.

import type {
  AttentionKind,
  SessionStatus,
  Session_Serialize,
  SessionsView_Serialize,
  Workspace_Serialize,
} from "../../../ainb-app/bindings/AppState";

/** Precedence, tightest first, as `AttentionKind`'s `Ord` in Rust. */
export const ATTENTION_ORDER: readonly AttentionKind[] = ["Ask", "Wait", "Approve", "Err", "Done"];

/** The kinds that want a person now: the footer's one "N need you" count
 * sums these, and a row ringing one of them is never also counted idle.
 * `Done` is not one: a finished turn is for reading, not answering. */
export const NEED_YOU: readonly AttentionKind[] = ["Ask", "Approve", "Wait", "Err"];

/** The kinds that block a turn: the header's attention badge counts these. */
export const BLOCKING: readonly AttentionKind[] = ["Ask", "Wait", "Approve"];

/** A row's lifecycle as the sidebar reads it. `unknown` is a status this build
 * has never seen (a host at another version): never guessed into `stopped`,
 * which would draw a live agent as exited. */
export type RowStatus = "running" | "idle" | "stopped" | "error" | "unknown";

export function rowStatus(status: SessionStatus): RowStatus {
  if (typeof status === "object") return "error";
  switch (status) {
    case "Running":
      return "running";
    case "Idle":
      return "idle";
    case "Stopped":
      return "stopped";
    // A host at another version may send a status this build does not know:
    // said as unknown, not guessed.
    default:
      return "unknown";
  }
}

/**
 * The attention ring a row paints, or `null` for none: the tightest kind of the
 * merged attention its frame row carries. The host merged it (daemon rows by
 * exact provider id, local hook events, the session's own error, an attached
 * row left silent), so the renderer only picks the kind to paint.
 */
export function ringFor(session: Session_Serialize): AttentionKind | null {
  let ring: AttentionKind | null = null;
  // Optional: a host at another version may not send it.
  for (const mark of session.attention ?? []) {
    // A kind this build does not know (a host at another version) is
    // skipped: `indexOf` would read -1 and rank it above every real kind.
    if (!ATTENTION_ORDER.includes(mark.kind)) continue;
    if (ring === null || ATTENTION_ORDER.indexOf(mark.kind) < ATTENTION_ORDER.indexOf(ring)) ring = mark.kind;
  }
  return ring;
}

/** The most characters a sidebar label draws. */
export const LABEL_CHARS = 80;

/**
 * A name as the sidebar may draw it: control and format characters removed
 * (a bidi override or an escape in a branch name cannot restyle the row) and
 * cut to `LABEL_CHARS` characters.
 */
export function label(text: string): string {
  return Array.from(text.replace(/[\p{Cc}\p{Cf}]/gu, ""))
    .slice(0, LABEL_CHARS)
    .join("");
}

/** Every session row the Sessions frame lists, across its workspaces. */
export function allSessions(view: SessionsView_Serialize | undefined): Session_Serialize[] {
  return view?.workspaces.flatMap((workspace) => workspace.sessions) ?? [];
}

/** The provider session id inside a `provider:session-id` key: `board.ts`'s
 * join, `status.ts`'s join, and nowhere else, so a third copy never drifts
 * from the other two. */
export function providerId(sessionKey: string): string {
  const at = sessionKey.indexOf(":");
  return at < 0 ? sessionKey : sessionKey.slice(at + 1);
}

/** A legacy key's exact tmux target (`session:window.pane`), or `null` for
 * any other key. `SessionKey::legacy` writes `legacy:<provider>:<target>:
 * <fingerprint>`; the target may itself hold `:`, the fingerprint never does. */
export function legacyTarget(sessionKey: string): string | null {
  return /^legacy:[^:]+:(.+):[^:]*$/.exec(sessionKey)?.[1] ?? null;
}

/** The tmux session a legacy key's target names: the part before the window,
 * which tmux never lets a session name contain. */
export function legacyTmuxSession(sessionKey: string): string | null {
  return legacyTarget(sessionKey)?.split(":")[0] ?? null;
}

/**
 * What a card is called when no session row names it. A legacy card's key is
 * `legacy:<provider>:<tmux target>:<fingerprint>` (`SessionKey::legacy`): its
 * name is the tmux target, the thing a person would recognise. Any other key
 * is `<provider>:<session id>`: the provider and the id's first 8 characters.
 * Never the raw key, which is an identifier and not a name.
 */
export function keyLabel(sessionKey: string): string {
  const legacy = legacyTarget(sessionKey);
  if (legacy !== null) return legacy;
  const at = sessionKey.indexOf(":");
  return at < 0 ? sessionKey : `${sessionKey.slice(0, at)} ${sessionKey.slice(at + 1, at + 9)}`;
}

/**
 * Whether `sessionId` is the session list's selected row.
 *
 * The frame carries only the rows the filter shows, and the selection by id
 * (#1180): an index would be into the reducer's full list, which the window
 * never sees.
 */
export function isSelected(view: SessionsView_Serialize | undefined, sessionId: string): boolean {
  return view?.selected_session_id === sessionId;
}

/** How many rows ring with `kind`: one header count. */
export function ringCount(view: SessionsView_Serialize | undefined, kind: AttentionKind): number {
  return allSessions(view).filter((session) => ringFor(session) === kind).length;
}

