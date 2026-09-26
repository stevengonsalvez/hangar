// In-memory transport for development and tests (`EXPO_PUBLIC_FAKE_WIRE=1`).
// Same interface as the native binding; nothing here touches a socket.

import { fromBase64 } from "../terminal/engine/protocol";
import { FIXTURES } from "../terminal/fixtures";
import type {
  AnswerOutcome,
  AttentionRow,
  FleetCursor,
  FloorDenied,
  FloorHolder,
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
  WireEvent,
} from "./types";

interface FakeStream {
  hostId: HostId;
  sessionKey: SessionKey;
  cols: number;
  rows: number;
  seq: number;
  epoch: number;
}

interface FakeHost {
  row: HostRow;
  sessions: SessionRow[];
  transcripts: Map<SessionKey, TranscriptEntry[]>;
  attention: Map<string, AttentionRow & { answeredBy?: string }>;
  revision: number;
  connected: boolean;
}

const HOST_A = "01K5A0000000000000000AAAAA";
const HOST_B = "01K5B0000000000000000BBBBB";

function session(hostId: HostId, key: string, name: string, state: string): SessionRow {
  return {
    hostId,
    sessionKey: key,
    name,
    state,
    provenance: "hook",
    tier: "live",
    lifecycleUpdatedAt: 1_700_000_000_000,
    sessionIncarnation: `inc-${key}`,
  };
}

export class FakeWire implements WireClient {
  private hosts_ = new Map<HostId, FakeHost>();
  private listeners = new Set<(ev: WireEvent) => void>();
  private log: LogLine[] = [];
  private nextOp = 1;
  /** Every `answer` call, for tests: `[opId, attentionId, answer, version]`. */
  readonly answers: { opId: string; attentionId: string; answer: string; version: number }[] = [];
  /** When set, the next `answer` is APPLIED and then its reply is lost (throws). */
  dropNextAnswer = false;
  /** Scope and capabilities hello would report; tests flip these. */
  info: HostInfo = { scope: { base: "mobile", admin: false }, capabilities: ["terminal.stream", "terminal.input"] };
  /** Every `terminal/input` call, for tests. */
  readonly inputs: { streamId: number; floorGen?: number; data: string }[] = [];
  readonly detached: number[] = [];
  private streams = new Map<number, FakeStream>();
  private nextStream = 1;
  /** The floor per session key: who holds it and its generation. */
  private floors = new Map<SessionKey, FloorState>();
  /** The op-id ledger: a retry under a known id replays the stored reply. */
  private ledger = new Map<string, { outcome: AnswerOutcome; ack: MutationAck }>();
  /** Dedupe ledger for prompts and interrupts. */
  private opLedger = new Map<string, MutationAck>();
  private transcriptSubs = new Map<string, Set<(e: TranscriptEntry) => void>>();
  /** How many answers actually reached a row (the effect count). */
  deliveries = 0;

  constructor(seed = true) {
    if (seed) {
      this.addHost(HOST_A, "laptop", "reachable");
      this.addHost(HOST_B, "server", "unreachable", 1_700_000_000_000);
      const a = this.hosts_.get(HOST_A)!;
      a.sessions.push(
        session(HOST_A, "claude:hangar", "hangar", "ask"),
        session(HOST_A, "codex:web", "web", "working"),
      );
      a.transcripts.set("claude:hangar", [
        { seq: 1, role: "user", text: "fix the flaky test", atMs: 1 },
        { seq: 2, role: "agent", text: "Which runner do you mean?", atMs: 2 },
      ]);
      a.attention.set("att-1", {
        hostId: HOST_A,
        id: "att-1",
        sessionId: "s-1",
        sessionKey: "claude:hangar",
        kind: "ask_user_question",
        version: 3,
        createdAt: 3,
        payload: { question: "Which runner do you mean?", options: ["ubuntu", "macos"] },
      });
    }
  }

  // test hooks --------------------------------------------------------------

  addHost(hostId: HostId, displayName: string, reachability: HostRow["reachability"], sinceMs?: number) {
    this.hosts_.set(hostId, {
      row: { hostId, displayName, reachability, sinceMs, scope: { base: "mobile", admin: false } },
      sessions: [],
      transcripts: new Map(),
      attention: new Map(),
      revision: 0,
      connected: false,
    });
  }

  /** Raise an ASK on a host: bumps the fleet revision and emits the event. */
  raise(row: Omit<AttentionRow, "hostId"> & { hostId?: HostId }): AttentionRow {
    const hostId = row.hostId ?? HOST_A;
    const host = this.host(hostId);
    const full: AttentionRow = { ...row, hostId };
    if (host.attention.has(full.id)) return full; // the same row twice is one event
    host.attention.set(full.id, full);
    host.revision += 1;
    if (host.connected) this.emit({ kind: "attention_raised", row: full });
    return full;
  }

  /** Commit N fleet events without an ASK, as the daemon does while we are away; `silent` skips the event. */
  advance(hostId: HostId, n: number, silent = false) {
    const host = this.host(hostId);
    host.revision += n;
    if (host.connected && !silent) this.emit({ kind: "fleet_revision", hostId, revision: host.revision });
  }

  /** The agent moved on: the session's lifecycle clock advances under the reader. */
  advanceTurn(hostId: HostId, sessionKey: SessionKey) {
    const host = this.host(hostId);
    host.sessions = host.sessions.map((s) =>
      s.sessionKey === sessionKey ? { ...s, lifecycleUpdatedAt: s.lifecycleUpdatedAt + 1 } : s,
    );
  }

  /** Someone else answered: the row retires and the event says who. */
  answeredElsewhere(hostId: HostId, attentionId: string, by: string) {
    const row = this.host(hostId).attention.get(attentionId);
    if (row) row.answeredBy = by;
    if (this.host(hostId).connected) this.emit({ kind: "attention_answered", hostId, attentionId, by });
  }

  /** The row disappears on the daemon without an event (session gone). */
  vanish(hostId: HostId, attentionId: string) {
    this.host(hostId).attention.delete(attentionId);
  }

  isConnected(hostId: HostId) {
    return this.host(hostId).connected;
  }

  /** A desktop takes the floor for this session out from under the phone. */
  floorTakenBy(sessionKey: SessionKey, label: string) {
    const prev = this.floors.get(sessionKey) ?? { floorGen: 0 };
    const next: FloorState = { holder: { principal: `local:${label}`, label, streamId: 0 }, floorGen: prev.floorGen + 1 };
    this.floors.set(sessionKey, next);
    for (const [id, st] of this.streams) if (st.sessionKey === sessionKey) this.frame(id, { kind: "floor", holder: next.holder, floorGen: next.floorGen });
  }

  /** Emit one frame on a stream, as the daemon would. */
  frame(streamId: number, frame: TerminalFrame) {
    const st = this.streams.get(streamId);
    if (!st) return;
    st.seq += frame.kind === "output" || frame.kind === "snapshot_chunk" ? frame.data.length : 0;
    this.emit({ kind: "terminal_frame", hostId: st.hostId, streamId, seq: st.seq, frame });
  }

  /** Feed lost: a gap then a fresh snapshot, as the contract promises (C-R2-6). */
  dropFeed(streamId: number) {
    this.frame(streamId, { kind: "data_gap", reason: "feed_lost" });
    this.snapshot(streamId);
  }

  private snapshot(streamId: number) {
    const st = this.streams.get(streamId);
    if (!st) return;
    st.epoch += 1;
    this.frame(streamId, { kind: "snapshot_start", cols: st.cols, rows: st.rows, epoch: st.epoch, chunks: 1 });
    this.frame(streamId, { kind: "snapshot_chunk", data: fromBase64(FIXTURES["f1-altscreen"]) });
    this.frame(streamId, { kind: "snapshot_end" });
  }

  private denied(sessionKey: SessionKey): FloorDenied {
    const f = this.floors.get(sessionKey) ?? { floorGen: 0 };
    return { kind: "floor_denied", holder: f.holder, floorGen: f.floorGen };
  }

  private holdsFloor(streamId: number): boolean {
    const st = this.streams.get(streamId);
    return !!st && this.floors.get(st.sessionKey)?.holder?.streamId === streamId;
  }

  /** The socket dropped (network, or a peer close with `code`). */
  dropConnection(hostId: HostId, code?: number, reason?: string) {
    this.host(hostId).connected = false;
    this.record(hostId, "close", code === undefined ? "network" : String(code));
    this.emit({ kind: "closed", hostId, code, reason });
  }

  /** The daemon asks the phone to start over from a snapshot. */
  resyncRequired(hostId: HostId) {
    this.emit({ kind: "fleet_resync_required", hostId });
  }

  /** Every call the real crate would refuse without a session. */
  private connected(hostId: HostId): FakeHost {
    const host = this.host(hostId);
    if (!host.connected) throw new Error(`not connected: ${hostId}`);
    return host;
  }

  // WireClient --------------------------------------------------------------

  async hosts() {
    return [...this.hosts_.values()].map((h) => h.row);
  }

  async parseOffer(uri: string): Promise<PairingOffer> {
    if (!uri.startsWith("ainb://pair#")) throw new Error("not an ainb pairing offer");
    const hostId = uri.slice("ainb://pair#".length, "ainb://pair#".length + 26);
    if (hostId.length !== 26) throw new Error("offer host id is not 26 characters");
    return { hostId, endpoints: [{ carrier: "lan", url: "ws://fake" }], expiresAtMs: Date.now() + 300_000 };
  }

  async pair(offer: PairingOffer, displayName: string): Promise<PairedHost> {
    if (!this.hosts_.has(offer.hostId)) this.addHost(offer.hostId, displayName, "reachable");
    this.record(offer.hostId, "paired", displayName);
    return { hostId: offer.hostId, deviceId: `dev-${offer.hostId.slice(-5)}`, scope: { base: "mobile", admin: false } };
  }

  async forget(hostId: HostId) {
    this.hosts_.delete(hostId);
  }

  async connect(hostId: HostId) {
    const host = this.host(hostId);
    if (host.row.reachability === "unreachable") throw new Error("unreachable");
    host.connected = true;
    this.record(hostId, "hello", "scope=mobile");
  }

  async close(hostId: HostId) {
    const host = this.host(hostId);
    host.connected = false;
    this.record(hostId, "close", "1000");
  }

  async hostInfo(hostId: HostId): Promise<HostInfo> {
    this.host(hostId);
    return this.info;
  }

  async terminalAttach(req: { hostId: HostId; sessionKey: SessionKey; cols?: number; rows?: number; wantInput?: boolean }): Promise<TerminalAttached> {
    this.connected(req.hostId);
    const streamId = this.nextStream++;
    const st: FakeStream = { hostId: req.hostId, sessionKey: req.sessionKey, cols: req.cols ?? 80, rows: req.rows ?? 24, seq: 0, epoch: 0 };
    this.streams.set(streamId, st);
    let floor = this.floors.get(req.sessionKey) ?? { floorGen: 0 };
    if (req.wantInput && !floor.holder) {
      floor = { holder: { principal: "device:fake", label: "phone", streamId }, floorGen: floor.floorGen + 1 };
      this.floors.set(req.sessionKey, floor);
    }
    queueMicrotask(() => this.snapshot(streamId));
    return { streamId, epoch: st.epoch, snapshotSeq: 0, cols: st.cols, rows: st.rows, floor, nativeClients: 1 };
  }

  async terminalDetach(hostId: HostId, streamId: number) {
    this.host(hostId);
    const st = this.streams.get(streamId);
    if (!st) return;
    this.detached.push(streamId);
    if (this.holdsFloor(streamId)) {
      const f = this.floors.get(st.sessionKey)!;
      this.floors.set(st.sessionKey, { floorGen: f.floorGen + 1 });
    }
    this.streams.delete(streamId);
  }

  async terminalInput(req: { hostId: HostId; streamId: number; floorGen?: number; data: string }) {
    const st = this.streams.get(req.streamId);
    if (!st) throw new Error("unknown stream");
    this.inputs.push({ streamId: req.streamId, floorGen: req.floorGen, data: req.data });
    let floor = this.floors.get(st.sessionKey) ?? { floorGen: 0 };
    if (!floor.holder && req.floorGen === undefined) {
      floor = { holder: { principal: "device:fake", label: "phone", streamId: req.streamId }, floorGen: floor.floorGen + 1 };
      this.floors.set(st.sessionKey, floor);
    }
    if (floor.holder?.streamId !== req.streamId || (req.floorGen !== undefined && req.floorGen !== floor.floorGen)) return this.denied(st.sessionKey);
    this.frame(req.streamId, { kind: "output", data: new TextEncoder().encode(req.data) }); // echo
    return { floorGen: floor.floorGen };
  }

  async terminalResize(req: { hostId: HostId; streamId: number; cols: number; rows: number }): Promise<TerminalResizeOutcome> {
    const st = this.streams.get(req.streamId);
    if (!st || !this.holdsFloor(req.streamId)) return { outcome: "not_applicable", windowSize: "latest" };
    st.cols = req.cols;
    st.rows = req.rows;
    this.frame(req.streamId, { kind: "resize", cols: req.cols, rows: req.rows });
    return { outcome: "applied", cols: req.cols, rows: req.rows };
  }

  async terminalFloor(req: { hostId: HostId; streamId: number; action: "acquire" | "release" | "take" }): Promise<FloorState | FloorDenied> {
    const st = this.streams.get(req.streamId);
    if (!st) throw new Error("unknown stream");
    const cur = this.floors.get(st.sessionKey) ?? { floorGen: 0 };
    const mine: FloorHolder = { principal: "device:fake", label: "phone", streamId: req.streamId };
    let next: FloorState;
    if (req.action === "release") next = { floorGen: cur.floorGen + 1 };
    else if (req.action === "take" || !cur.holder || cur.holder.streamId === req.streamId) next = { holder: mine, floorGen: cur.floorGen + 1 };
    else return this.denied(st.sessionKey);
    this.floors.set(st.sessionKey, next);
    for (const [id, s] of this.streams) if (s.sessionKey === st.sessionKey) this.frame(id, { kind: "floor", holder: next.holder, floorGen: next.floorGen });
    return next;
  }

  async rosterStatus(hostId: HostId) {
    return [...this.connected(hostId).sessions];
  }

  async subscribeFleet(hostId: HostId, afterRevision?: number): Promise<FleetCursor> {
    const host = this.connected(hostId);
    if (afterRevision === undefined) return { revision: host.revision, replayState: "complete" };
    return { revision: host.revision, replayState: afterRevision <= host.revision ? "complete" : "snapshot_reset" };
  }

  async subscribeAttention(hostId: HostId) {
    return [...this.connected(hostId).attention.values()].filter((r) => !r.answeredBy);
  }

  async mintOpId() {
    return `op-${this.nextOp++}`;
  }

  async answer(req: { hostId: HostId; attentionId: string; answer: string; version: number; opId: string }) {
    const { opId } = req;
    this.answers.push({ opId, attentionId: req.attentionId, answer: req.answer, version: req.version });
    const replay = this.ledger.get(opId);
    if (replay) return { outcome: replay.outcome, ack: { ...replay.ack, outcome: "replayed" as const } };
    const host = this.connected(req.hostId);
    const row = host.attention.get(req.attentionId);
    const ack: MutationAck = { outcome: "created", status: "accepted", receipt: "delivered" };
    let outcome: AnswerOutcome;
    if (!row) outcome = { kind: "no_target", reason: "no_target" };
    else if (row.answeredBy) outcome = { kind: "already_answered", by: row.answeredBy };
    else if (row.version !== req.version) {
      ack.status = "rejected";
      ack.reason = "already_answered_by";
      outcome = { kind: "rejected", reason: "already_answered_by" };
    } else {
      row.answeredBy = "device:fake";
      this.deliveries += 1;
      outcome = { kind: "delivered", via: "fake" };
    }
    this.ledger.set(opId, { outcome, ack });
    if (this.dropNextAnswer) {
      // The daemon did its work; only the reply never made it back.
      this.dropNextAnswer = false;
      throw new Error("reply lost");
    }
    return { outcome, ack };
  }

  /** Every prompt and interrupt call, for tests. */
  readonly prompts: { opId: string; sessionKey: SessionKey; text: string }[] = [];

  async sendPrompt(req: { hostId: HostId; sessionKey: SessionKey; text: string; lifecycleUpdatedAt: number; opId: string }): Promise<MutationAck> {
    const host = this.connected(req.hostId);
    this.prompts.push({ opId: req.opId, sessionKey: req.sessionKey, text: req.text });
    const seen = this.opLedger.get(req.opId);
    if (seen) return { ...seen, outcome: "replayed" };
    const s = host.sessions.find((x) => x.sessionKey === req.sessionKey);
    let ack: MutationAck;
    if (!s) ack = { outcome: "created", status: "rejected", reason: "no_target" };
    else if (s.lifecycleUpdatedAt !== req.lifecycleUpdatedAt) ack = { outcome: "created", status: "rejected", reason: "turn_advanced" };
    else {
      const t = host.transcripts.get(req.sessionKey) ?? [];
      const entry: TranscriptEntry = { seq: (t.at(-1)?.seq ?? 0) + 1, role: "user", text: req.text, atMs: Date.now() };
      t.push(entry);
      host.transcripts.set(req.sessionKey, t);
      this.emit({ kind: "transcript_line", hostId: req.hostId, sessionKey: req.sessionKey, entry });
      for (const cb of this.transcriptSubs.get(`${req.hostId}/${req.sessionKey}`) ?? []) cb(entry);
      ack = { outcome: "created", status: "accepted", receipt: "delivered" };
    }
    this.opLedger.set(req.opId, ack);
    return ack;
  }

  async interrupt(req: { hostId: HostId; sessionKey: SessionKey; sessionIncarnation: string; opId: string }): Promise<MutationAck> {
    const host = this.connected(req.hostId);
    const seen = this.opLedger.get(req.opId);
    if (seen) return { ...seen, outcome: "replayed" };
    const s = host.sessions.find((x) => x.sessionKey === req.sessionKey);
    let ack: MutationAck;
    if (!s) ack = { outcome: "created", status: "rejected", reason: "no_target" };
    else if (s.sessionIncarnation !== req.sessionIncarnation) ack = { outcome: "created", status: "rejected", reason: "incarnation_mismatch" };
    else ack = { outcome: "created", status: "accepted" };
    this.opLedger.set(req.opId, ack);
    return ack;
  }

  async transcriptPage(hostId: HostId, sessionKey: SessionKey, beforeSeq?: number) {
    const t = this.connected(hostId).transcripts.get(sessionKey) ?? [];
    return beforeSeq === undefined ? [...t] : t.filter((e) => e.seq < beforeSeq);
  }

  subscribeTranscript(hostId: HostId, sessionKey: SessionKey, cb: (entry: TranscriptEntry) => void): Unsubscribe {
    const key = `${hostId}/${sessionKey}`;
    const set = this.transcriptSubs.get(key) ?? new Set();
    set.add(cb);
    this.transcriptSubs.set(key, set);
    return () => set.delete(cb);
  }

  async connectionLog() {
    return [...this.log];
  }

  async deviceKeyFingerprint() {
    return "SHA256:fake-device-key";
  }

  onEvent(cb: (ev: WireEvent) => void): Unsubscribe {
    this.listeners.add(cb);
    return () => this.listeners.delete(cb);
  }

  // internals ---------------------------------------------------------------

  private host(hostId: HostId): FakeHost {
    const h = this.hosts_.get(hostId);
    if (!h) throw new Error(`unknown host ${hostId}`);
    return h;
  }

  private emit(ev: WireEvent) {
    for (const cb of this.listeners) cb(ev);
  }

  private record(hostId: HostId, event: string, detail?: string) {
    this.log.push({ atMs: Date.now(), hostId, event, detail });
  }
}

export const FAKE_HOST_A = HOST_A;
export const FAKE_HOST_B = HOST_B;
