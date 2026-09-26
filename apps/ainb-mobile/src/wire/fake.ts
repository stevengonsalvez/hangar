// In-memory transport for development and tests (`EXPO_PUBLIC_FAKE_WIRE=1`).
// Same interface as the native binding; nothing here touches a socket.

import type {
  AnswerOutcome,
  AttentionRow,
  FleetCursor,
  HostId,
  HostRow,
  LogLine,
  MutationAck,
  PairedHost,
  PairingOffer,
  SessionKey,
  SessionRow,
  TranscriptEntry,
  Unsubscribe,
  WireClient,
  WireEvent,
} from "./types";

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
