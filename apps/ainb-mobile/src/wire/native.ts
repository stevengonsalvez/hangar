// The native adapter: `WireClient` over the ubrn-generated bindings for
// `ainb-wire-mobile` (lane E, #111's contract). It replaces `FakeWire` when
// the binding is linked. One `MobileHost` object per connected host with a
// pull event loop on the native side becomes the hostId-keyed, push-event
// interface the screens use. Every record is mapped here, one function per
// record, so nothing outside src/wire/ reads a wire shape and the app keeps
// no copy of the crate's meaning: latch strings come from `latchValues()`,
// the redial verdict from `Closed { retryable, retry_after_ms }`, transcript
// text and role from the crate's own decoding. The app parses no JSON.
//
// The generated module is loaded lazily through `loadBindings` so a build
// without it fails at first use with one clear message, not at import.

import type {
  AnswerOutcome,
  AttentionRow,
  FleetCursor,
  FloorDenied,
  FloorState,
  HostId,
  HostInfo,
  HostRow,
  LogLine,
  MutationAck,
  PairedHost,
  PairingOffer,
  SessionKey,
  SessionRow,
  TerminalAttached,
  TerminalFrame,
  TerminalResizeOutcome,
  TranscriptEntry,
  Unsubscribe,
  WireClient,
  WireErrorKind,
  WireEvent,
} from "./types";
import { PeerCloseError } from "./types";

// ---------------------------------------------------------------------------
// The generated surface this adapter relies on. Field and method names are
// ubrn's camelCase rendering of the crate's uniffi exports (api.rs,
// records.rs, pairing.rs, terminal.rs). Enums arrive as `{ tag, inner }`.
// ---------------------------------------------------------------------------

/** A uniffi enum value as ubrn renders it. */
export interface Tagged<T = Record<string, unknown>> {
  tag: string;
  inner?: T;
}

export interface NativePairingRecord {
  hostId: string;
  hostStaticPubkey: ArrayBuffer | Uint8Array;
  endpoints: { carrier: string; url: string }[];
  deviceId: string;
  displayName: string;
  scope: string;
  admin: boolean;
  expiresAtMs: bigint | number;
  pairedAtMs: bigint | number;
  repair?: string;
  notice?: string;
}

export interface NativeHelloSummary {
  selectedProtocol: number;
  capabilities: string[];
  daemonVersion?: string;
  hostId?: string;
  scope?: string;
  admin: boolean;
  deviceExpiresAtMs?: bigint | number;
}

export interface NativeRosterRow {
  sessionKey: string;
  provider: string;
  displayName?: string;
  cwd: string;
  state: string;
  provenance: string;
  tier: string;
  hasOpenRequest: boolean;
  lifecycleUpdatedAt: bigint | number;
  processStartFingerprint?: string;
  version: bigint | number;
  readRevision: bigint | number;
}

export interface NativeAttentionRecord {
  id: string;
  sessionId: string;
  workspaceId?: string;
  kind: string;
  version: bigint | number;
  payload: { question?: string; options: { label: string; description: string }[]; text?: string; message?: string };
  degraded: boolean;
  createdAt: bigint | number;
}

export interface NativeMutationReceipt {
  status: string;
  outcome?: string;
  reason?: string;
  receipt?: string;
}

/** `TranscriptChunkRecord`: `role` and `text` are decoded by the crate. */
export interface NativeTranscriptChunk {
  ingestOrder: bigint | number;
  eventId: string;
  sessionKey: string;
  eventType: string;
  role: string;
  text?: string;
  payload: string;
  observedAt: bigint | number;
}

export interface NativeMobileHost {
  hello(): NativeHelloSummary;
  advertises(capability: string): boolean;
  canType(): boolean;
  rosterStatus(): Promise<{ rows: NativeRosterRow[]; readRevision: bigint | number; readAtMs: bigint | number }>;
  subscribeFleet(afterRevision: bigint | number): Promise<{
    headRevision: bigint | number;
    replayComplete: boolean;
    resetReason?: string;
  }>;
  attentionList(): Promise<NativeAttentionRecord[]>;
  subscribeAttention(): Promise<NativeAttentionRecord[]>;
  answer(attentionId: string, answer: string, version: bigint | number, opId: string): Promise<{
    opId: string;
    outcome: Tagged;
    ack?: NativeMutationReceipt;
  }>;
  sendPrompt(sessionKey: string, text: string, lifecycleUpdatedAt: bigint | number, opId: string): Promise<{
    opId: string;
    messageId: string;
    ack?: NativeMutationReceipt;
  }>;
  interrupt(sessionKey: string, version: bigint | number, processStartFingerprint: string, opId: string): Promise<{
    opId: string;
    receiptStatus: string;
    ack?: NativeMutationReceipt;
  }>;
  transcriptPage(sessionKey: string, afterOrder: bigint | number | undefined, limit: number): Promise<{
    chunks: NativeTranscriptChunk[];
    nextAfterOrder?: bigint | number;
    truncated: boolean;
  }>;
  subscribeTranscript(sessionKey: string, afterOrder?: bigint | number): Promise<bigint | number | undefined>;
  terminalAttach(sessionKey: string, cols: number | undefined, rows: number | undefined, wantInput: boolean, scrollbackRows: number | undefined): Promise<{
    streamId: bigint | number;
    epoch: bigint | number;
    snapshotSeq: bigint | number;
    cols: number;
    rows: number;
    floor: { holder?: { principal: string; label: string; streamId: bigint | number }; floorGen: bigint | number };
    nativeClients: number;
  }>;
  terminalDetach(streamId: bigint | number): Promise<void>;
  terminalInput(streamId: bigint | number, data: ArrayBuffer | Uint8Array, floorGen: bigint | number | undefined, opId: string): Promise<Tagged>;
  terminalFloor(streamId: bigint | number, action: string, opId: string): Promise<Tagged>;
  terminalResize(streamId: bigint | number, cols: number, rows: number): Promise<Tagged>;
  nextEvent(): Promise<Tagged>;
  close(): void;
  isClosed(): boolean;
  connectionLog(limit: number): { tMs: bigint | number; event: string; detail: string }[];
}

export interface Bindings {
  parseOffer(uri: string): { hostId: string; endpoints: { carrier: string; url: string }[]; inviteId: string; expiresAtMs: bigint | number };
  pair(uri: string, displayName: string, custodyDir: string, logDir: string): Promise<NativePairingRecord>;
  listPairings(custodyDir: string): NativePairingRecord[];
  forgetPairing(custodyDir: string, hostId: string): void;
  connectHost(params: { hostId: string; custodyDir: string; logDir: string }): Promise<NativeMobileHost>;
  mintOpId(): string;
  backoffDelayMs(attempt: number, logDir?: string): bigint | number;
  readConnectionLog(logDir: string, limit: number): { tMs: bigint | number; event: string; detail: string }[];
  deviceKeyFingerprint(custodyDir: string): string;
  latchValues(): string[];
}

/** Where the generated module lives once ubrn has run (ubrn.config.yaml). */
export const BINDINGS_MODULE = "../../modules/ainb-wire-mobile";

let bindings: Bindings | undefined;

/** The linked binding, loaded on first use; tests inject their own. */
export function loadBindings(): Bindings {
  if (bindings) return bindings;
  try {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    bindings = require(BINDINGS_MODULE) as Bindings;
  } catch (e) {
    throw new Error(`ainb-wire-mobile is not linked (${String(e)}); run with EXPO_PUBLIC_FAKE_WIRE=1`);
  }
  return bindings;
}

export function setBindings(b: Bindings | undefined) {
  bindings = b;
}

// ---------------------------------------------------------------------------
// Mapping, one function per record. Numbers cross uniffi as bigint for u64
// and i64; every value the app holds is below 2^53 (seq, revisions, epoch
// ms), so they are narrowed here.
// ---------------------------------------------------------------------------

export const num = (v: bigint | number | undefined): number | undefined =>
  v === undefined ? undefined : Number(v);
const n0 = (v: bigint | number): number => Number(v);
const bytes = (b: ArrayBuffer | Uint8Array): Uint8Array => (b instanceof Uint8Array ? b : new Uint8Array(b));

/** The D18 codes a mutation answers with instead of a result. */
export const MUTATION_REJECTED = -32008;
export const MUTATION_UNKNOWN = -32009;

/**
 * A thrown `Rpc` error that is really a mutation verdict: `-32008` with its
 * `reason` is the ledger rejecting (turn advanced, already answered, floor
 * denied ...), `-32009` is the ledger unable to say. Both are outcomes the
 * app renders, never throws.
 */
export function mutationVerdict(e: unknown): MutationAck | undefined {
  const err = e as { tag?: string; inner?: { code?: number | bigint; reason?: string; message?: string } };
  if (err?.tag !== "Rpc") return undefined;
  const code = Number(err.inner?.code);
  const reason = err.inner?.reason ?? err.inner?.message;
  if (code === MUTATION_REJECTED) return { status: "rejected", reason };
  if (code === MUTATION_UNKNOWN) return { status: "unknown", reason };
  return undefined;
}

/** The crate's `WireError` (a thrown uniffi error) as a `PeerCloseError`, one to one. */
export function toPeerCloseError(e: unknown): PeerCloseError {
  const err = e as { tag?: string; inner?: Record<string, unknown>; message?: string };
  const inner = err?.inner ?? {};
  const kindOf: Record<string, WireErrorKind> = {
    Connect: "connect",
    PeerChanged: "peer_changed",
    Handshake: "handshake",
    Protocol: "protocol",
    Rpc: "rpc",
    Timeout: "timeout",
    Closed: "closed",
    Offer: "offer",
    Custody: "custody",
    NotPaired: "not_paired",
  };
  const kind = kindOf[err?.tag ?? ""] ?? "protocol";
  if (kind === "closed") {
    return new PeerCloseError("closed", {
      code: num(inner.code as bigint | number | undefined),
      reason: String(inner.reason ?? ""),
      retryable: Boolean(inner.retryable),
      retryAfterMs: num(inner.retryAfterMs as bigint | number | undefined),
    });
  }
  return new PeerCloseError(kind, {
    reason: String(inner.message ?? inner.hostId ?? err?.message ?? ""),
    retryable: kind === "connect" || kind === "timeout",
  });
}

type ScopeBase = NonNullable<HostRow["scope"]>["base"];

export function toHostRow(r: NativePairingRecord, reachable: boolean, sinceMs?: number): HostRow {
  return {
    hostId: r.hostId,
    displayName: r.displayName,
    reachability: reachable ? "reachable" : "unreachable",
    sinceMs: reachable ? undefined : sinceMs,
    scope: { base: r.scope as ScopeBase, admin: r.admin },
    repair: r.repair,
    notice: r.notice,
  };
}

/** A session row plus the optimistic `version` the crate's `interrupt` fences on. */
export type SessionRowWithVersion = SessionRow & { version: number };

export function toSessionRow(hostId: HostId, r: NativeRosterRow): SessionRowWithVersion {
  return {
    hostId,
    sessionKey: r.sessionKey,
    name: r.displayName ?? r.sessionKey,
    state: r.state,
    provenance: r.provenance,
    tier: r.tier,
    lifecycleUpdatedAt: n0(r.lifecycleUpdatedAt),
    sessionIncarnation: r.processStartFingerprint ?? "",
    version: n0(r.version),
  };
}

export function toAttentionRow(hostId: HostId, r: NativeAttentionRecord): AttentionRow {
  return {
    hostId,
    id: r.id,
    sessionId: r.sessionId,
    kind: r.kind,
    version: n0(r.version),
    createdAt: n0(r.createdAt),
    payload: {
      question: r.payload.question ?? r.payload.message,
      options: r.payload.options.length ? r.payload.options.map((o) => o.label) : undefined,
      text: r.payload.text,
    },
  };
}

export function toAck(a: NativeMutationReceipt | undefined): MutationAck | undefined {
  if (!a) return undefined;
  return {
    status: a.status as MutationAck["status"],
    outcome: a.outcome as MutationAck["outcome"],
    reason: a.reason,
    receipt: a.receipt as MutationAck["receipt"],
  };
}

export function toAnswerOutcome(t: Tagged): AnswerOutcome {
  const inner = (t.inner ?? {}) as Record<string, string>;
  switch (t.tag) {
    case "Delivered":
      return { kind: "delivered", via: inner.via ?? "" };
    case "AlreadyAnswered":
      return { kind: "already_answered", by: inner.by ?? "" };
    case "Ambiguous":
      return { kind: "ambiguous", reason: inner.reason ?? "" };
    case "NoTarget":
      return { kind: "no_target", reason: inner.reason ?? "" };
    case "DeliveryFailed":
      return { kind: "delivery_failed", reason: inner.reason ?? "" };
    default:
      return { kind: "unknown", reason: t.tag };
  }
}

const toHolder = (h?: { principal: string; label: string; streamId: bigint | number }) =>
  h ? { principal: h.principal, label: h.label, streamId: n0(h.streamId) } : undefined;

export function toFloorState(f: { holder?: { principal: string; label: string; streamId: bigint | number }; floorGen: bigint | number }): FloorState {
  return { holder: toHolder(f.holder), floorGen: n0(f.floorGen) };
}

/** `TerminalInputOutcome` / `TerminalFloorOutcome`: a refusal is a value, never a throw. */
export function toFloorDenied(t: Tagged): FloorDenied | undefined {
  if (t.tag !== "FloorDenied") return undefined;
  const inner = (t.inner ?? {}) as { holder?: { principal: string; label: string; streamId: bigint | number }; floorGen: bigint | number };
  return { kind: "floor_denied", holder: toHolder(inner.holder), floorGen: n0(inner.floorGen) };
}

export function toResizeOutcome(t: Tagged): TerminalResizeOutcome {
  const inner = (t.inner ?? {}) as Record<string, unknown>;
  if (t.tag === "Applied") return { outcome: "applied", cols: Number(inner.cols), rows: Number(inner.rows) };
  if (t.tag === "NotApplicable") return { outcome: "not_applicable", windowSize: String(inner.windowSize ?? "") };
  return { outcome: "unknown" };
}

type GapReason = Extract<TerminalFrame, { kind: "data_gap" }>["reason"];

export function toTerminalFrame(t: Tagged): TerminalFrame {
  const inner = (t.inner ?? {}) as Record<string, unknown>;
  switch (t.tag) {
    case "SnapshotStart":
      return { kind: "snapshot_start", cols: Number(inner.cols), rows: Number(inner.rows), epoch: Number(inner.epoch), chunks: Number(inner.chunks) };
    case "SnapshotChunk":
      return { kind: "snapshot_chunk", data: bytes(inner.data as ArrayBuffer) };
    case "SnapshotEnd":
      return { kind: "snapshot_end" };
    case "Output":
      return { kind: "output", data: bytes(inner.data as ArrayBuffer) };
    case "Resize":
      return { kind: "resize", cols: Number(inner.cols), rows: Number(inner.rows) };
    case "DataGap":
      return { kind: "data_gap", reason: String(inner.reason) as GapReason, droppedBytes: num(inner.droppedBytes as bigint | number | undefined) };
    case "Floor":
      return { kind: "floor", holder: toHolder(inner.holder as { principal: string; label: string; streamId: bigint | number } | undefined), floorGen: Number(inner.floorGen) };
    case "Presence":
      return { kind: "presence", nativeClients: Number(inner.nativeClients) };
    case "Closed":
      return { kind: "closed", reason: String(inner.reason ?? "") };
    default:
      return { kind: "unknown" };
  }
}

/** One chunk as the crate decoded it: no JSON is parsed here. */
export function toTranscriptEntry(c: NativeTranscriptChunk): TranscriptEntry {
  const role = (["user", "agent", "tool", "system"] as const).find((r) => r === c.role) ?? "system";
  return { seq: n0(c.ingestOrder), role, text: c.text ?? c.eventType, atMs: n0(c.observedAt) };
}

/**
 * A crate `WireEvent` (from `nextEvent`) as an app event, or `undefined` for
 * one the app ignores. `AttentionRaised` is not mapped here: the crate's
 * nudge carries no version, so the pump fetches the real row instead.
 */
export function toWireEvent(hostId: HostId, t: Tagged): WireEvent | undefined {
  const inner = (t.inner ?? {}) as Record<string, unknown>;
  switch (t.tag) {
    case "FleetRevision":
      return { kind: "fleet_revision", hostId, revision: Number((inner.event as { revision: bigint | number }).revision) };
    case "FleetResyncRequired":
      return { kind: "fleet_resync_required", hostId };
    case "AttentionAnswered":
      return { kind: "attention_answered", hostId, attentionId: String(inner.attentionId), by: String(inner.by) };
    case "TranscriptChunk": {
      const chunk = inner.chunk as NativeTranscriptChunk;
      return { kind: "transcript_line", hostId, sessionKey: chunk.sessionKey, entry: toTranscriptEntry(chunk) };
    }
    case "TerminalFrame":
      return { kind: "terminal_frame", hostId, streamId: Number(inner.streamId), seq: Number(inner.seq), frame: toTerminalFrame(inner.frame as Tagged) };
    case "Lagged":
      return { kind: "lagged", hostId };
    case "Closed":
      return {
        kind: "closed",
        hostId,
        code: num(inner.code as bigint | number | undefined),
        reason: String(inner.reason ?? ""),
        retryable: Boolean(inner.retryable),
        retryAfterMs: num(inner.retryAfterMs as bigint | number | undefined),
      };
    default:
      return undefined;
  }
}

// ---------------------------------------------------------------------------
// The client.
// ---------------------------------------------------------------------------

export interface NativeWireOptions {
  /** The app-data directory the crate keeps secrets and the pairing index under. */
  custodyDir: string;
  /** The app-data directory the connection log lives under. */
  logDir: string;
  bindings?: Bindings;
}

interface Live {
  host: NativeMobileHost;
  pumping: boolean;
}

/** The page the transcript screen asks for: the daemon caps a page at 100. */
export const TRANSCRIPT_PAGE = 100;

/** `WireClient` over the linked `ainb-wire-mobile` binding. */
export class NativeWire implements WireClient {
  private readonly b: Bindings;
  private readonly custodyDir: string;
  private readonly logDir: string;
  private readonly live = new Map<HostId, Live>();
  /** A dial in flight per host: concurrent `connect` callers share it. */
  private readonly dialing = new Map<HostId, Promise<void>>();
  private readonly lastSeen = new Map<HostId, number>();
  private readonly listeners = new Set<(ev: WireEvent) => void>();
  private readonly transcriptTaps = new Map<string, Set<(entry: TranscriptEntry) => void>>();

  constructor(opts: NativeWireOptions) {
    this.b = opts.bindings ?? loadBindings();
    this.custodyDir = opts.custodyDir;
    this.logDir = opts.logDir;
  }

  private emit(ev: WireEvent) {
    for (const cb of this.listeners) cb(ev);
    if (ev.kind === "transcript_line") {
      for (const cb of this.transcriptTaps.get(`${ev.hostId}\u0000${ev.sessionKey}`) ?? []) cb(ev.entry);
    }
  }

  private hostOf(hostId: HostId): NativeMobileHost {
    const l = this.live.get(hostId);
    if (!l || l.host.isClosed()) throw new PeerCloseError("not_paired", { reason: `${hostId} is not connected` });
    return l.host;
  }

  /**
   * Pull the crate's events for one host until it closes; every one is
   * pushed to the listeners. A raised attention row is fetched whole so the
   * banner carries its real `version`. A pull that fails is a broken loop,
   * not a network loss: the close it reports is not retryable, the app
   * reconnects deliberately.
   */
  private async pump(hostId: HostId, l: Live) {
    if (l.pumping) return;
    l.pumping = true;
    for (;;) {
      let ev: Tagged;
      try {
        ev = await l.host.nextEvent();
      } catch (e) {
        this.emit({ kind: "closed", hostId, reason: `event loop failed: ${String(e)}`, retryable: false });
        break;
      }
      if (ev.tag === "AttentionRaised") {
        const id = String((ev.inner ?? {}).attentionId);
        try {
          const row = (await l.host.attentionList()).find((r) => r.id === id);
          // A row already answered by the time we look is not news.
          if (row) this.emit({ kind: "attention_raised", row: toAttentionRow(hostId, row) });
        } catch {
          // The list read failed; the next subscribeAttention snapshot carries the row.
        }
        continue;
      }
      const mapped = toWireEvent(hostId, ev);
      if (mapped) this.emit(mapped);
      if (ev.tag === "Closed") break;
    }
    l.pumping = false;
    this.live.delete(hostId);
    this.lastSeen.set(hostId, Date.now());
  }

  async hosts(): Promise<HostRow[]> {
    return this.b.listPairings(this.custodyDir).map((r) => {
      const l = this.live.get(r.hostId);
      return toHostRow(r, Boolean(l && !l.host.isClosed()), this.lastSeen.get(r.hostId));
    });
  }

  async parseOffer(uri: string): Promise<PairingOffer> {
    try {
      const o = this.b.parseOffer(uri);
      return { hostId: o.hostId, endpoints: o.endpoints as PairingOffer["endpoints"], expiresAtMs: n0(o.expiresAtMs) };
    } catch (e) {
      throw toPeerCloseError(e);
    }
  }

  async pair(uri: string, displayName: string): Promise<PairedHost> {
    try {
      const r = await this.b.pair(uri, displayName, this.custodyDir, this.logDir);
      return { hostId: r.hostId, deviceId: r.deviceId, scope: { base: r.scope as ScopeBase, admin: r.admin } };
    } catch (e) {
      throw toPeerCloseError(e);
    }
  }

  async forget(hostId: HostId): Promise<void> {
    await this.close(hostId);
    this.b.forgetPairing(this.custodyDir, hostId);
  }

  /** One socket per host: a connect while a dial is in flight joins that dial. */
  async connect(hostId: HostId): Promise<void> {
    const existing = this.live.get(hostId);
    if (existing && !existing.host.isClosed()) return;
    const inFlight = this.dialing.get(hostId);
    if (inFlight) return inFlight;
    const dial = (async () => {
      let host: NativeMobileHost;
      try {
        host = await this.b.connectHost({ hostId, custodyDir: this.custodyDir, logDir: this.logDir });
      } catch (e) {
        this.lastSeen.set(hostId, Date.now());
        throw toPeerCloseError(e);
      }
      const l: Live = { host, pumping: false };
      this.live.set(hostId, l);
      void this.pump(hostId, l);
    })();
    this.dialing.set(hostId, dial);
    try {
      await dial;
    } finally {
      this.dialing.delete(hostId);
    }
  }

  /** A close during a dial waits for that dial, then closes what it produced. */
  async close(hostId: HostId): Promise<void> {
    const inFlight = this.dialing.get(hostId);
    if (inFlight) await inFlight.catch(() => undefined);
    const l = this.live.get(hostId);
    if (!l) return;
    l.host.close();
  }

  async hostInfo(hostId: HostId): Promise<HostInfo> {
    const h = this.hostOf(hostId).hello();
    return { scope: h.scope ? { base: h.scope as ScopeBase, admin: h.admin } : undefined, capabilities: h.capabilities };
  }

  async rosterStatus(hostId: HostId): Promise<SessionRowWithVersion[]> {
    const r = await this.hostOf(hostId).rosterStatus();
    return r.rows.map((row) => toSessionRow(hostId, row));
  }

  /**
   * No `afterRevision` is the first connect: it asks for the snapshot, that
   * is the roster's `read_revision` as the cursor, so nothing is replayed
   * from 0. With a cursor, the daemon replays from it.
   */
  async subscribeFleet(hostId: HostId, afterRevision?: number): Promise<FleetCursor> {
    const host = this.hostOf(hostId);
    const from = afterRevision ?? n0((await host.rosterStatus()).readRevision);
    const r = await host.subscribeFleet(from);
    return { revision: n0(r.headRevision), replayState: r.replayComplete ? "complete" : "snapshot_reset" };
  }

  async subscribeAttention(hostId: HostId): Promise<AttentionRow[]> {
    const rows = await this.hostOf(hostId).subscribeAttention();
    return rows.map((r) => toAttentionRow(hostId, r));
  }

  async mintOpId(): Promise<string> {
    return this.b.mintOpId();
  }

  async answer(req: { hostId: HostId; attentionId: string; answer: string; version: number; opId: string }) {
    try {
      const r = await this.hostOf(req.hostId).answer(req.attentionId, req.answer, req.version, req.opId);
      return { outcome: toAnswerOutcome(r.outcome), ack: toAck(r.ack) };
    } catch (e) {
      const verdict = mutationVerdict(e);
      if (!verdict) throw toPeerCloseError(e);
      const outcome: AnswerOutcome =
        verdict.status === "rejected"
          ? { kind: "rejected", reason: verdict.reason ?? "" }
          : { kind: "unknown", reason: verdict.reason ?? "" };
      return { outcome, ack: verdict };
    }
  }

  async sendPrompt(req: { hostId: HostId; sessionKey: SessionKey; text: string; lifecycleUpdatedAt: number; opId: string }): Promise<MutationAck> {
    try {
      const r = await this.hostOf(req.hostId).sendPrompt(req.sessionKey, req.text, req.lifecycleUpdatedAt, req.opId);
      return toAck(r.ack) ?? { status: "accepted" };
    } catch (e) {
      const verdict = mutationVerdict(e);
      if (!verdict) throw toPeerCloseError(e);
      return verdict;
    }
  }

  /** `fleet/action { interrupt }` fenced on the row the user saw: its `version` and incarnation. */
  async interrupt(req: { hostId: HostId; sessionKey: SessionKey; sessionIncarnation: string; version: number; opId: string }): Promise<MutationAck> {
    try {
      const r = await this.hostOf(req.hostId).interrupt(req.sessionKey, req.version, req.sessionIncarnation, req.opId);
      return toAck(r.ack) ?? { status: "accepted" };
    } catch (e) {
      const verdict = mutationVerdict(e);
      if (!verdict) throw toPeerCloseError(e);
      return verdict;
    }
  }

  /**
   * Without `beforeSeq`: the newest page (the daemon's cap, 100 rows). With
   * it: the page of entries older than `beforeSeq`, which the wire cannot
   * fetch yet: `fleet/transcript_list` reads forward from `after_order` and
   * caps at 100, so any window below `beforeSeq` returns the OLDEST rows of
   * the session, not the ones just above the cursor. Until the daemon takes
   * a `before_order`, an older page is empty and the screen shows what it
   * has. ponytail: swap the `[]` for one call with `before_order` when it
   * lands.
   */
  async transcriptPage(hostId: HostId, sessionKey: SessionKey, beforeSeq?: number): Promise<TranscriptEntry[]> {
    const host = this.hostOf(hostId);
    if (beforeSeq !== undefined) return [];
    const tail = await host.transcriptPage(sessionKey, undefined, TRANSCRIPT_PAGE);
    return tail.chunks.map(toTranscriptEntry);
  }

  subscribeTranscript(hostId: HostId, sessionKey: SessionKey, cb: (entry: TranscriptEntry) => void): Unsubscribe {
    const key = `${hostId}\u0000${sessionKey}`;
    const taps = this.transcriptTaps.get(key) ?? new Set();
    taps.add(cb);
    this.transcriptTaps.set(key, taps);
    void this.hostOf(hostId).subscribeTranscript(sessionKey, undefined);
    return () => {
      taps.delete(cb);
      if (taps.size === 0) this.transcriptTaps.delete(key);
    };
  }

  async terminalAttach(req: { hostId: HostId; sessionKey: SessionKey; cols?: number; rows?: number; wantInput?: boolean }): Promise<TerminalAttached> {
    const a = await this.hostOf(req.hostId).terminalAttach(req.sessionKey, req.cols, req.rows, req.wantInput ?? false, undefined);
    return { streamId: n0(a.streamId), epoch: n0(a.epoch), snapshotSeq: n0(a.snapshotSeq), cols: a.cols, rows: a.rows, floor: toFloorState(a.floor), nativeClients: a.nativeClients };
  }

  async terminalDetach(hostId: HostId, streamId: number): Promise<void> {
    await this.hostOf(hostId).terminalDetach(streamId);
  }

  /** Receipt tier: `opId` is the app's, minted before the first send and reused on a retry. */
  async terminalInput(req: { hostId: HostId; streamId: number; floorGen?: number; data: string; opId: string }): Promise<{ floorGen: number } | FloorDenied> {
    const t = await this.hostOf(req.hostId).terminalInput(req.streamId, new TextEncoder().encode(req.data), req.floorGen, req.opId);
    return toFloorDenied(t) ?? { floorGen: Number(((t.inner ?? {}) as { floorGen: bigint | number }).floorGen) };
  }

  async terminalResize(req: { hostId: HostId; streamId: number; cols: number; rows: number }): Promise<TerminalResizeOutcome> {
    return toResizeOutcome(await this.hostOf(req.hostId).terminalResize(req.streamId, req.cols, req.rows));
  }

  /** Dedupe tier: `opId` is the app's, reused on a retry. */
  async terminalFloor(req: { hostId: HostId; streamId: number; action: "acquire" | "release" | "take"; opId: string }): Promise<FloorState | FloorDenied> {
    const t = await this.hostOf(req.hostId).terminalFloor(req.streamId, req.action, req.opId);
    return toFloorDenied(t) ?? toFloorState(((t.inner ?? {}) as { floor: Parameters<typeof toFloorState>[0] }).floor);
  }

  backoffDelayMs(attempt: number, retryAfterMs?: number): number {
    const crate = n0(this.b.backoffDelayMs(attempt, this.logDir));
    return retryAfterMs !== undefined ? Math.max(crate, retryAfterMs) : crate;
  }

  async connectionLog(): Promise<LogLine[]> {
    return this.b.readConnectionLog(this.logDir, 200).map((e) => ({ atMs: n0(e.tMs), event: e.event, detail: e.detail }));
  }

  async deviceKeyFingerprint(): Promise<string> {
    return this.b.deviceKeyFingerprint(this.custodyDir);
  }

  onEvent(cb: (ev: WireEvent) => void): Unsubscribe {
    this.listeners.add(cb);
    return () => this.listeners.delete(cb);
  }
}
