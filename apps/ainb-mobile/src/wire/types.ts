// The phone's view of one paired daemon: the `MobileHost` facade (C-M1-1).
//
// These records mirror the frozen proto shapes (docs/contracts/v2-next.md)
// one to one so the app never reads wire JSON itself (C-M1-3): every method
// here returns a decoded record, and every event arrives decoded. When the
// ubrn bindings for `ainb-wire-mobile` land, this file becomes a re-export of
// the generated types; nothing outside src/wire/ may declare a wire shape.

export type HostId = string;
export type SessionKey = string;
export type Reachability = "reachable" | "unreachable";

export interface HostRow {
  hostId: HostId;
  displayName: string;
  reachability: Reachability;
  /** Set when unreachable: when the host was last seen, epoch ms. */
  sinceMs?: number;
  scope?: DeviceScope;
  /**
   * The pairing record latched a close that only a new pairing clears:
   * 4403 (revoked) or 4401 after token expiry. Kept by the crate so it
   * survives a restart; a 4503 rescope never sets it (T9).
   */
  repair?: "revoked" | "expired";
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
  | { kind: "closed"; hostId: HostId; code: number }
  | { kind: "terminal_frame"; hostId: HostId; streamId: number; seq: number; frame: TerminalFrame };

export type Unsubscribe = () => void;

export interface WireClient {
  hosts(): Promise<HostRow[]>;
  parseOffer(uri: string): Promise<PairingOffer>;
  pair(offer: PairingOffer, displayName: string): Promise<PairedHost>;
  forget(hostId: HostId): Promise<void>;

  connect(hostId: HostId): Promise<void>;
  close(hostId: HostId): Promise<void>;
  hostInfo(hostId: HostId): Promise<HostInfo>;

  rosterStatus(hostId: HostId): Promise<SessionRow[]>;
  subscribeFleet(hostId: HostId, afterRevision: number): Promise<FleetCursor>;
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

  sendPrompt(req: {
    hostId: HostId;
    sessionKey: SessionKey;
    text: string;
    lifecycleUpdatedAt: number;
  }): Promise<MutationAck>;
  interrupt(req: {
    hostId: HostId;
    sessionKey: SessionKey;
    sessionIncarnation: string;
  }): Promise<MutationAck>;

  transcriptPage(hostId: HostId, sessionKey: SessionKey, beforeSeq?: number): Promise<TranscriptEntry[]>;

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

  connectionLog(): Promise<LogLine[]>;
  deviceKeyFingerprint(): Promise<string>;

  onEvent(cb: (ev: WireEvent) => void): Unsubscribe;
}
