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
   * A close that only a new pairing clears, per `peer_close.rs` (T9):
   * 4403 `revoked` (revoked OR token expired, "latch re-pair") and 4401
   * `identity` (unauthenticated: "identity changed, re-pair"). Kept with
   * the pairing record so it survives a restart; a 4503 rescope never sets it.
   */
  /** Also `peer_changed`: the host's static key no longer matches the pinned one, a fresh offer and never a redial. Read from lane E's pairing record (#98 `mark_repair`), no copy kept. */
  repair?: "revoked" | "identity" | "peer_changed";
  /**
   * A state nobody should redial through: 4409 (update one side), a code this
   * build does not know, or a host whose static key no longer matches the
   * pinned one (`peer_changed`: ask the operator for a fresh offer).
   */
  notice?: "update_required" | "unknown_close";
}

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
  /** The socket closed; `code` is the peer close code when there was one (T9). */
  | { kind: "closed"; hostId: HostId; code?: number; reason?: string }
  /** The event stream fell behind: resubscribe from the cursor. */
  | { kind: "lagged"; hostId: HostId }
  /** The daemon asks for a fresh snapshot: resubscribe without a cursor. */
  | { kind: "fleet_resync_required"; hostId: HostId };

export type Unsubscribe = () => void;

/** Why a dial failed. Only `network` and a retryable `close` code may ever be redialled. */
export type PeerCloseKind = "network" | "close" | "peer_changed" | "not_paired" | "custody" | "protocol" | "offer";

/**
 * A `connect` that failed, typed so the lifecycle can decide about a redial
 * without guessing. Lane E's `connect_host` answers with a `WireError` and no
 * close event; the adapter maps it to this class as follows:
 *
 * | WireError                | kind           | code  | redial                     |
 * |--------------------------|----------------|-------|----------------------------|
 * | Network / Io / Timeout   | `network`      |       | yes, backoff               |
 * | Closed { code, reason }  | `close`        | code  | only 1013, 4429, 4503      |
 * | Revoked (or expired)     | `close`        | 4403  | no, row shows re-pair      |
 * | Unauthenticated          | `close`        | 4401  | no, row shows re-pair      |
 * | Incompatible             | `close`        | 4409  | no, row shows update       |
 * | PeerChanged (key differs)| `peer_changed` |       | no, row latches re-pair    |
 * | NotPaired                | `not_paired`   |       | no                         |
 * | Custody (key store)      | `custody`      |       | no                         |
 * | Protocol (bad frame)     | `protocol`     |       | no                         |
 * | Offer (bad or expired)   | `offer`        |       | no                         |
 *
 * Anything that is not one of these, including a plain `Error`, is treated
 * as "stop": the loop is fail-closed. The latch itself (`HostRow.repair`,
 * `notice`) lives in lane E's pairing record (#98 `mark_repair`) and is read
 * from `hosts()`; the app keeps no copy.
 */
export class PeerCloseError extends Error {
  readonly kind: PeerCloseKind;
  readonly code?: number;
  readonly reason?: string;
  constructor(kind: PeerCloseKind, code?: number, reason?: string) {
    super(kind === "close" && code !== undefined ? `peer closed ${code}${reason ? `: ${reason}` : ""}` : `${kind}${reason ? `: ${reason}` : ""}`);
    this.name = "PeerCloseError";
    this.kind = kind;
    this.code = code;
    this.reason = reason;
  }

  /** A network failure: the one failure the loop redials on its own. */
  static network(reason?: string): PeerCloseError {
    return new PeerCloseError("network", undefined, reason);
  }

  /** A peer close with a T9 code. */
  static closed(code: number, reason?: string): PeerCloseError {
    return new PeerCloseError("close", code, reason);
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
  /** `fleet/action { interrupt }`, same op id rule. */
  interrupt(req: {
    hostId: HostId;
    sessionKey: SessionKey;
    sessionIncarnation: string;
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
  terminalInput(req: { hostId: HostId; streamId: number; floorGen?: number; data: string }): Promise<{ floorGen: number } | FloorDenied>;
  terminalResize(req: { hostId: HostId; streamId: number; cols: number; rows: number }): Promise<TerminalResizeOutcome>;
  terminalFloor(req: { hostId: HostId; streamId: number; action: "acquire" | "release" | "take" }): Promise<FloorState | FloorDenied>;

  /** Lane E's `backoff_delay_ms`: jittered, 1 s doubling to a 60 s ceiling; a host's `retry-after` is a floor under it. */
  backoffDelayMs(attempt: number, retryAfterSecs?: number): number;

  connectionLog(): Promise<LogLine[]>;
  deviceKeyFingerprint(): Promise<string>;

  onEvent(cb: (ev: WireEvent) => void): Unsubscribe;
}
