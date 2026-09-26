// The phone's view of one paired daemon: the `MobileHost` facade (C-M1-1).
//
// These records mirror the frozen proto shapes (docs/contracts/v2-next.md)
// one to one so the app never reads wire JSON itself (C-M1-3): every method
// here returns a decoded record, and every event arrives decoded. The native
// side (lane E's `MobileHost`, one object per host with a pull event loop)
// is wrapped by an adapter in src/wire/ that presents this hostId-keyed,
// push-event interface; nothing outside src/wire/ may declare a wire shape.

export type HostId = string;
export type SessionKey = string;
/** T4: `stale` is a host whose last heartbeat is late; `unknown` is one this build cannot classify. */
export type Reachability = "reachable" | "unreachable" | "stale" | "unknown";

export interface HostRow {
  hostId: HostId;
  displayName: string;
  reachability: Reachability;
  /** Set when unreachable: when the host was last seen, epoch ms. */
  sinceMs?: number;
  scope?: DeviceScope;
  /**
   * The re-pair latch, verbatim from lane E's `PairingRecord.repair`
   * (`Option<String>`, set by `mark_repair`; constants in pairing.rs):
   * `revoked` (4403, revoked or expired), `unauthenticated` (4401),
   * `peer_changed` (the host's static key is not the pinned one). Read from
   * `hosts()` on every call; the app keeps no copy. Any value, known or not,
   * means no redial and a path to the pair screen.
   */
  repair?: RepairLatch;
  /**
   * A parked state from the same record (`Option<String>`): `incompatible`
   * (4409, update one side) or `unknown_code` (a close code this build does
   * not know). Read from `hosts()`, no copy kept. Any value means no redial.
   */
  notice?: HostNotice;
}

/** The crate's latch strings (pairing.rs `REPAIR_*`), plus any newer value, which still compiles and still blocks. */
export type RepairLatch = "revoked" | "unauthenticated" | "peer_changed" | (string & {});
/** The crate's parked strings (pairing.rs `NOTICE_*`), plus any newer value. */
export type HostNotice = "incompatible" | "unknown_code" | (string & {});

export type BaseScope = "desktop" | "mobile" | "mobile+type" | "unknown";
export interface DeviceScope {
  base: BaseScope;
  admin: boolean;
}

/** Decoded `ainb://pair#...` offer (T7). Secrets stay inside the crate. */
export interface PairingOffer {
  hostId: HostId;
  endpoints: { carrier: "tailnet" | "lan" | "ssh-l" | "unknown"; url: string }[];
  expiresAtMs: number;
}

/** `pair` refuses with this when the host's static key differs from the pinned one. */
export const PEER_CHANGED = "peer_changed";

export interface PairedHost {
  hostId: HostId;
  deviceId: string;
  scope: DeviceScope;
}

/** One `fleet/roster_status` row, the D14 tuple the TUI shows. */
export interface SessionRow {
  hostId: HostId;
  sessionKey: SessionKey;
  name: string;
  state: string;
  provenance: string;
  tier: string;
  /** Fence for `fleet/message_send` (`Fence::LifecycleUpdatedAt`). */
  lifecycleUpdatedAt: number;
  /** Fence for `fleet/action{interrupt}` (`Fence::SessionIncarnation`). */
  sessionIncarnation: string;
  /** The row version the user saw; sent back with `interrupt` so a stale view is refused, never a null. */
  version: number;
}

/** `AttentionRow` with its payload already decoded by the crate. */
export interface AttentionRow {
  hostId: HostId;
  id: string;
  sessionId: string;
  sessionKey?: SessionKey;
  kind: string;
  /** The `attention/answer` fence (`Fence::AttentionVersion`). */
  version: number;
  createdAt: number;
  payload: AttentionPayload;
}

export interface AttentionPayload {
  question?: string;
  /** Option labels; the answer is the option's 1-based index as text. */
  options?: string[];
  text?: string;
}

export type AnswerOutcome =
  | { kind: "delivered"; via: string }
  | { kind: "already_answered"; by: string }
  | { kind: "ambiguous"; reason: string }
  | { kind: "no_target"; reason: string }
  | { kind: "delivery_failed"; reason: string }
  /** `MUTATION_REJECTED` (-32008) with its `data.reason`. */
  | { kind: "rejected"; reason: string }
  /** `MUTATION_UNKNOWN` (-32009). */
  | { kind: "unknown"; reason: string };

export interface MutationAck {
  /** `created` on first execution, `replayed` when the ledger served a retry. */
  outcome?: "created" | "replayed";
  status: "accepted" | "rejected" | "unknown";
  reason?: string;
  /** The JSON-RPC error code behind a refusal: `-32008` MUTATION_REJECTED (stale fence or version, `reason` says which), `-32009` MUTATION_UNKNOWN. */
  code?: number;
  receipt?: "claimed" | "writing" | "delivered" | "failed" | "unknown";
}

export interface TranscriptEntry {
  seq: number;
  role: "user" | "agent" | "tool" | "system";
  text: string;
  atMs: number;
}

export interface FleetCursor {
  revision: number;
  replayState: "complete" | "snapshot_reset";
}

export interface LogLine {
  atMs: number;
  hostId?: HostId;
  event: string;
  detail?: string;
}

// Terminal stream (RECONCILED T15 to T17, M5 to M11). Frame `data` arrives
// already base64-decoded by the crate.
export interface FloorHolder {
  principal: string;
  label: string;
  streamId: number;
}
export interface FloorState {
  holder?: FloorHolder;
  floorGen: number;
}
export interface TerminalAttached {
  streamId: number;
  epoch: number;
  snapshotSeq: number;
  cols: number;
  rows: number;
  floor: FloorState;
  nativeClients: number;
}
export type DataGapReason = "paused" | "feed_lost" | "dropped" | "session_gone" | "unknown";
export type TerminalFrame =
  | { kind: "snapshot_start"; cols: number; rows: number; epoch: number; chunks: number }
  | { kind: "snapshot_chunk"; data: Uint8Array }
  | { kind: "snapshot_end" }
  | { kind: "output"; data: Uint8Array }
  | { kind: "resize"; cols: number; rows: number }
  | { kind: "data_gap"; reason: DataGapReason; droppedBytes?: number }
  | { kind: "floor"; holder?: FloorHolder; floorGen: number }
  | { kind: "presence"; nativeClients: number }
  | { kind: "closed"; reason: string }
  | { kind: "unknown" };
/** `MUTATION_REJECTED` with `reason: floor_denied` (M11). */
export interface FloorDenied {
  kind: "floor_denied";
  holder?: FloorHolder;
  floorGen: number;
}
export type TerminalResizeOutcome =
  | { outcome: "applied"; cols: number; rows: number }
  | { outcome: "not_applicable"; windowSize: string }
  | { outcome: "unknown" };

/** What hello told us about this host: gates the type toggle (C-R2-8). */
export interface HostInfo {
  scope?: DeviceScope;
  capabilities: string[];
}

export type WireEvent =
  | { kind: "attention_raised"; row: AttentionRow }
  | { kind: "attention_answered"; hostId: HostId; attentionId: string; by: string }
  | { kind: "fleet_revision"; hostId: HostId; revision: number }
  | { kind: "reachability"; hostId: HostId; reachability: Reachability; sinceMs?: number }
  | { kind: "transcript_line"; hostId: HostId; sessionKey: SessionKey; entry: TranscriptEntry }
  | { kind: "terminal_frame"; hostId: HostId; streamId: number; seq: number; frame: TerminalFrame }
  /**
   * The socket closed (the crate's `WireEvent::Closed`): `code` when the host
   * sent one, the crate's verdict `retryable` (false for a close the phone
   * asked for), and `retryAfterMs` from the close reason.
   */
  | { kind: "closed"; hostId: HostId; code?: number; reason: string; retryable: boolean; retryAfterMs?: number }
  /** The event stream fell behind: resubscribe from the cursor. */
  | { kind: "lagged"; hostId: HostId }
  /** The daemon asks for a fresh snapshot: resubscribe without a cursor. */
  | { kind: "fleet_resync_required"; hostId: HostId };

export type Unsubscribe = () => void;

/** The crate's `WireError` variant a failed call maps to (lane E #111). */
export type WireErrorKind =
  | "connect"
  | "peer_changed"
  | "handshake"
  | "protocol"
  | "rpc"
  | "timeout"
  | "closed"
  | "offer"
  | "custody"
  | "not_paired";

/**
 * A failed `connect` (or any facade call), carrying the crate's own verdict
 * on whether a redial makes sense. The adapter maps `WireError` one to one:
 *
 * | WireError                                  | kind           | retryable | retryAfterMs   |
 * |--------------------------------------------|----------------|-----------|----------------|
 * | Connect { message }                        | `connect`      | true      |                |
 * | Timeout { method }                         | `timeout`      | true      |                |
 * | Closed { code, reason, retryable, retry_after_ms } | `closed` | as given  | as given       |
 * | PeerChanged                                | `peer_changed` | false     |                |
 * | Handshake { message }                      | `handshake`    | false     |                |
 * | Protocol { message }                       | `protocol`     | false     |                |
 * | Rpc { code, message, reason, data }        | `rpc`          | false     |                |
 * | Offer { message }                          | `offer`        | false     |                |
 * | Custody { message }                        | `custody`      | false     |                |
 * | NotPaired { host_id }                      | `not_paired`   | false     |                |
 *
 * `Closed` with `code: None, retryable: true` is a network loss and redials;
 * the crate alone decides `retryable` (peer_close.rs T9 for a coded close,
 * never for a close the phone asked for itself) and `retry_after_ms` (the
 * `retry-after=<s>` close reason). The app parses nothing and keeps no code
 * table of its own. The latch itself lives in the crate's pairing record and
 * is read back as `HostRow.repair`.
 */
export class PeerCloseError extends Error {
  readonly kind: WireErrorKind;
  readonly code?: number;
  readonly reason?: string;
  readonly retryable: boolean;
  readonly retryAfterMs?: number;
  constructor(kind: WireErrorKind, opts: { code?: number; reason?: string; retryable?: boolean; retryAfterMs?: number } = {}) {
    super(
      kind === "closed"
        ? `session closed (${opts.code ?? "no code"})${opts.reason ? `: ${opts.reason}` : ""}`
        : `${kind}${opts.reason ? `: ${opts.reason}` : ""}`,
    );
    this.name = "PeerCloseError";
    this.kind = kind;
    this.code = opts.code;
    this.reason = opts.reason;
    this.retryable = opts.retryable ?? (kind === "connect" || kind === "timeout");
    this.retryAfterMs = opts.retryAfterMs;
  }
}

/** `instanceof` plus the name, since two copies of this module can exist in one bundle. */
export function isPeerCloseError(e: unknown): e is PeerCloseError {
  return e instanceof PeerCloseError || (typeof e === "object" && e !== null && (e as { name?: unknown }).name === "PeerCloseError");
}

export interface WireClient {
  hosts(): Promise<HostRow[]>;
  /** Display-only decode of an offer (no secret crosses into JS). */
  parseOffer(uri: string): Promise<PairingOffer>;
  /** Redeem the offer URI itself: the crate reads the invite secret from it (lane E `pair(uri, ..)`). */
  pair(uri: string, displayName: string): Promise<PairedHost>;
  forget(hostId: HostId): Promise<void>;

  /** Rejects with a `PeerCloseError` carrying the close code when the peer refuses; a plain error is a network failure. */
  connect(hostId: HostId): Promise<void>;
  close(hostId: HostId): Promise<void>;
  hostInfo(hostId: HostId): Promise<HostInfo>;

  rosterStatus(hostId: HostId): Promise<SessionRow[]>;
  /** No `afterRevision` asks for a snapshot; with one, the daemon replays from it. */
  subscribeFleet(hostId: HostId, afterRevision?: number): Promise<FleetCursor>;
  subscribeAttention(hostId: HostId): Promise<AttentionRow[]>;

  /**
   * A fresh 128-bit op id from the crate's CSPRNG (`OpId::from_bytes`). Minted
   * BEFORE the send so a lost reply retries under the same id and the ledger
   * dedupes it.
   */
  mintOpId(): Promise<string>;

  /** `attention/answer` in the receipt tier, fenced on the row's `version`. */
  answer(req: {
    hostId: HostId;
    attentionId: string;
    answer: string;
    version: number;
    opId: string;
  }): Promise<{ outcome: AnswerOutcome; ack?: MutationAck }>;

  /** `fleet/message_send`, op id minted by `mintOpId` before the first send and reused on retry. */
  sendPrompt(req: {
    hostId: HostId;
    sessionKey: SessionKey;
    text: string;
    lifecycleUpdatedAt: number;
    opId: string;
  }): Promise<MutationAck>;
  /** `fleet/action { interrupt }`, same op id rule; `version` is the row version the user acted on. */
  interrupt(req: {
    hostId: HostId;
    sessionKey: SessionKey;
    sessionIncarnation: string;
    version: number;
    opId: string;
  }): Promise<MutationAck>;

  transcriptPage(hostId: HostId, sessionKey: SessionKey, beforeSeq?: number): Promise<TranscriptEntry[]>;
  /** `fleet/transcript_subscribe`: live lines for one session until unsubscribed. */
  subscribeTranscript(hostId: HostId, sessionKey: SessionKey, cb: (entry: TranscriptEntry) => void): Unsubscribe;

  /**
   * Terminal stream. `terminal/ack` is the crate's job (it counts the decoded
   * bytes), so it has no facade method. Input and floor calls answer the
   * floor refusal as a value, not a throw.
   */
  terminalAttach(req: {
    hostId: HostId;
    sessionKey: SessionKey;
    cols?: number;
    rows?: number;
    wantInput?: boolean;
  }): Promise<TerminalAttached>;
  terminalDetach(hostId: HostId, streamId: number): Promise<void>;
  /** Receipt-tier (M8): every batch carries an op id minted by `mintOpId`. */
  terminalInput(req: { hostId: HostId; streamId: number; floorGen?: number; data: string; opId: string }): Promise<{ floorGen: number } | FloorDenied>;
  terminalResize(req: { hostId: HostId; streamId: number; cols: number; rows: number }): Promise<TerminalResizeOutcome>;
  /** Dedupe tier (M9): every floor action carries an op id. */
  terminalFloor(req: { hostId: HostId; streamId: number; action: "acquire" | "release" | "take"; opId: string }): Promise<FloorState | FloorDenied>;

  /** Lane E's `backoff_delay_ms`: jittered, 1 s doubling to a 60 s ceiling; the host's `retry_after_ms` is a floor under it. */
  backoffDelayMs(attempt: number, retryAfterMs?: number): number;

  connectionLog(): Promise<LogLine[]>;
  deviceKeyFingerprint(): Promise<string>;

  onEvent(cb: (ev: WireEvent) => void): Unsubscribe;
}
