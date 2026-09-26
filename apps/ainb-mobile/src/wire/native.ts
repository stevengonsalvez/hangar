// The native adapter: `WireClient` over the ubrn-generated bindings for
// `ainb-wire-mobile` (lane E, #111's contract). It replaces `FakeWire` when
// the binding is linked. One `MobileHost` object per connected host with a
// pull event loop on the native side becomes the hostId-keyed, push-event
// interface the screens use. Every record is mapped here, one function per
// record, so nothing outside src/wire/ reads a wire shape and the app keeps
// no copy of the crate's meaning: latch strings come from `latchValues()`,
// the redial verdict from `Closed { retryable, retry_after_ms }`.
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
    chunks: { ingestOrder: bigint | number; eventId: string; sessionKey: string; eventType: string; payload: string; observedAt: bigint | number }[];
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

export function toHostRow(r: NativePairingRecord, reachable: boolean, sinceMs?: number): HostRow {
  return {
    hostId: r.hostId,
    displayName: r.displayName,
    reachability: reachable ? "reachable" : "unreachable",
    sinceMs: reachable ? undefined : sinceMs,
    scope: { base: r.scope as HostRow["scope"] extends infer S ? (S extends { base: infer B } ? B : never) : never, admin: r.admin },
    repair: r.repair,
    notice: r.notice,
  } as HostRow;
}

export function toSessionRow(hostId: HostId, r: NativeRosterRow): SessionRow {
  return {
    hostId,
    sessionKey: r.sessionKey,
    name: r.displayName ?? r.sessionKey,
    state: r.state,
    provenance: r.provenance,
    tier: r.tier,
    lifecycleUpdatedAt: n0(r.lifecycleUpdatedAt),
    sessionIncarnation: r.processStartFingerprint ?? "",
  } as SessionRow;
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
      return { kind: "data_gap", reason: String(inner.reason) as TerminalFrame extends { kind: "data_gap"; reason: infer R } ? R : never, droppedBytes: num(inner.droppedBytes as bigint | number | undefined) } as TerminalFrame;
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

/** One decoded transcript chunk as a transcript entry; the role is the ACP kind's last segment. */
export function toTranscriptEntry(c: { ingestOrder: bigint | number; eventType: string; payload: string; observedAt: bigint | number }): TranscriptEntry {
  const kind = c.eventType.split(".").pop() ?? "";
  const role: TranscriptEntry["role"] = kind.includes("user") ? "user" : kind.includes("tool") ? "tool" : kind.includes("agent") ? "agent" : "system";
  let text = c.payload;
  try {
    const p = JSON.parse(c.payload) as Record<string, unknown>;
    const candidate = p.text ?? p.content ?? p.message;
    if (typeof candidate === "string") text = candidate;
  } catch {
    // The chunk body is shown as it came.
  }
  return { seq: n0(c.ingestOrder), role, text, atMs: n0(c.observedAt) };
}

/** A crate `WireEvent` (from `nextEvent`) as an app event, or `undefined` for one the app ignores. */
export function toWireEvent(hostId: HostId, t: Tagged): WireEvent | undefined {
  const inner = (t.inner ?? {}) as Record<string, unknown>;
  switch (t.tag) {
    case "FleetRevision":
      return { kind: "fleet_revision", hostId, revision: Number((inner.event as { revision: bigint | number }).revision) };
    case "FleetResyncRequired":
      return { kind: "fleet_resync_required", hostId };
    case "AttentionRaised":
      return {
        kind: "attention_raised",
        row: {
          hostId,
          id: String(inner.attentionId),
          sessionId: String(inner.sessionId),
          kind: String(inner.kind),
          version: 0,
          createdAt: Number(inner.createdAt),
          payload: {},
        },
      };
    case "AttentionAnswered":
      return { kind: "attention_answered", hostId, attentionId: String(inner.attentionId), by: String(inner.by) };
    case "TranscriptChunk":
      return { kind: "transcript_line", hostId, sessionKey: String((inner.chunk as { sessionKey: string }).sessionKey), entry: toTranscriptEntry(inner.chunk as Parameters<typeof toTranscriptEntry>[0]) };
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

/** `WireClient` over the linked `ainb-wire-mobile` binding. */
export class NativeWire implements WireClient {
  private readonly b: Bindings;
  private readonly custodyDir: string;
  private readonly logDir: string;
  private readonly live = new Map<HostId, Live>();
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

  /** Pull the crate's events for one host until it closes; every one is pushed to the listeners. */
  private async pump(hostId: HostId, l: Live) {
    if (l.pumping) return;
    l.pumping = true;
    for (;;) {
      let ev: Tagged;
      try {
        ev = await l.host.nextEvent();
      } catch (e) {
        this.emit({ kind: "closed", hostId, reason: String(e), retryable: true });
        break;
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
      return { hostId: r.hostId, deviceId: r.deviceId, scope: { base: r.scope as PairedHost["scope"]["base"], admin: r.admin } };
    } catch (e) {
      throw toPeerCloseError(e);
    }
  }

  async forget(hostId: HostId): Promise<void> {
    await this.close(hostId);
    this.b.forgetPairing(this.custodyDir, hostId);
  }

  async connect(hostId: HostId): Promise<void> {
    const existing = this.live.get(hostId);
    if (existing && !existing.host.isClosed()) return;
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
  }

  async close(hostId: HostId): Promise<void> {
    const l = this.live.get(hostId);
    if (!l) return;
    l.host.close();
  }

  async hostInfo(hostId: HostId): Promise<HostInfo> {
    const h = this.hostOf(hostId).hello();
    return { scope: h.scope ? { base: h.scope as HostInfo["scope"] extends infer S ? (S extends { base: infer B } ? B : never) : never, admin: h.admin } as HostInfo["scope"] : undefined, capabilities: h.capabilities };
  }

  async rosterStatus(hostId: HostId): Promise<SessionRow[]> {
    const r = await this.hostOf(hostId).rosterStatus();
    return r.rows.map((row) => toSessionRow(hostId, row));
  }

  async subscribeFleet(hostId: HostId, afterRevision?: number): Promise<FleetCursor> {
    const r = await this.hostOf(hostId).subscribeFleet(afterRevision ?? 0);
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
    const r = await this.hostOf(req.hostId).answer(req.attentionId, req.answer, req.version, req.opId);
    return { outcome: toAnswerOutcome(r.outcome), ack: toAck(r.ack) };
  }

  async sendPrompt(req: { hostId: HostId; sessionKey: SessionKey; text: string; lifecycleUpdatedAt: number; opId: string }): Promise<MutationAck> {
    const r = await this.hostOf(req.hostId).sendPrompt(req.sessionKey, req.text, req.lifecycleUpdatedAt, req.opId);
    return toAck(r.ack) ?? { status: "accepted" };
  }

  async interrupt(req: { hostId: HostId; sessionKey: SessionKey; sessionIncarnation: string; opId: string }): Promise<MutationAck> {
    // The row's optimistic version rides with the roster; the crate's
    // interrupt takes it explicitly, so the adapter reads it back first.
    const rows = await this.hostOf(req.hostId).rosterStatus();
    const row = rows.rows.find((r) => r.sessionKey === req.sessionKey);
    const r = await this.hostOf(req.hostId).interrupt(req.sessionKey, row ? row.version : 0, req.sessionIncarnation, req.opId);
    return toAck(r.ack) ?? { status: "accepted" };
  }

  async transcriptPage(hostId: HostId, sessionKey: SessionKey, beforeSeq?: number): Promise<TranscriptEntry[]> {
    const page = await this.hostOf(hostId).transcriptPage(sessionKey, beforeSeq, 200);
    return page.chunks.map(toTranscriptEntry);
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

  async terminalInput(req: { hostId: HostId; streamId: number; floorGen?: number; data: string }): Promise<{ floorGen: number } | FloorDenied> {
    const t = await this.hostOf(req.hostId).terminalInput(req.streamId, new TextEncoder().encode(req.data), req.floorGen, this.b.mintOpId());
    return toFloorDenied(t) ?? { floorGen: Number(((t.inner ?? {}) as { floorGen: bigint | number }).floorGen) };
  }

  async terminalResize(req: { hostId: HostId; streamId: number; cols: number; rows: number }): Promise<TerminalResizeOutcome> {
    return toResizeOutcome(await this.hostOf(req.hostId).terminalResize(req.streamId, req.cols, req.rows));
  }

  async terminalFloor(req: { hostId: HostId; streamId: number; action: "acquire" | "release" | "take" }): Promise<FloorState | FloorDenied> {
    const t = await this.hostOf(req.hostId).terminalFloor(req.streamId, req.action, this.b.mintOpId());
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
