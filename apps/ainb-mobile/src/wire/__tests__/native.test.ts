// The native adapter against a scripted binding: every mapping the screens
// depend on, and the five latch strings compared to the crate's exported
// constants (`latchValues()`), so the app keeps no copy of their meaning.

import type { Bindings, NativeMobileHost, NativeTranscriptChunk, Tagged } from "../native";
import { NativeWire, toPeerCloseError, toWireEvent } from "../native";
import type { HostNotice, RepairLatch, WireEvent } from "../types";
import { isPeerCloseError } from "../types";

function must<T>(v: T | undefined, what: string): T {
  if (v === undefined) throw new Error(`missing ${what}`);
  return v;
}

/** What the crate exports as `latch_values()`, in its documented order. */
const CRATE_LATCH_VALUES = ["revoked", "unauthenticated", "peer_changed", "incompatible", "unknown_code"];

/** The app's literals, written here once so a drift in types.ts fails this test. */
const APP_REPAIR: RepairLatch[] = ["revoked", "unauthenticated", "peer_changed"];
const APP_NOTICE: HostNotice[] = ["incompatible", "unknown_code"];

/** A thrown crate `WireError::Rpc`, as ubrn renders it. */
const rpcError = (code: number, reason: string, message = "mutation refused"): Tagged => ({
  tag: "Rpc",
  inner: { code, message, reason },
});

/** Every call the scripted host saw, with what it was handed. */
interface Calls {
  subscribeFleet: number[];
  answer: { version: number; opId: string }[];
  sendPrompt: { opId: string }[];
  interrupt: { version: number; fingerprint: string; opId: string }[];
  transcriptPage: { afterOrder?: number; limit: number }[];
  terminalInput: string[];
  terminalFloor: string[];
  attentionList: number;
}

interface HostOpts {
  closed?: boolean;
  /** The op the ledger refuses, and how. */
  refuse?: { code: number; reason: string };
  /** `nextEvent` throws once the queue is empty. */
  pumpFails?: boolean;
  /** A global transcript: `ingest_order` values this session has. */
  transcript?: number[];
}

function scriptedHost(events: Tagged[], opts: HostOpts = {}): { host: NativeMobileHost; calls: Calls } {
  const queue = [...events];
  let closed = opts.closed ?? false;
  const calls: Calls = { subscribeFleet: [], answer: [], sendPrompt: [], interrupt: [], transcriptPage: [], terminalInput: [], terminalFloor: [], attentionList: 0 };
  const refuse = () => {
    if (opts.refuse) throw rpcError(opts.refuse.code, opts.refuse.reason);
  };
  const chunk = (order: number): NativeTranscriptChunk => ({
    ingestOrder: BigInt(order),
    eventId: `e${order}`,
    sessionKey: "claude:s-1",
    eventType: "acp.agent_message",
    role: "agent",
    text: `line ${order}`,
    payload: '{"opaque":true}',
    observedAt: 2n,
  });
  // With no scripted event left, `nextEvent` waits like the crate's pump
  // until `close()` produces the local close.
  let waiter: ((t: Tagged) => void) | undefined;
  const localClose: Tagged = { tag: "Closed", inner: { code: undefined, reason: "closed by client", retryable: false } };
  const host: NativeMobileHost = {
    hello: () => ({ selectedProtocol: 1, capabilities: ["hangar.scopes", "terminal.input"], hostId: "h1", scope: "mobile+type", admin: false }),
    advertises: (c) => c === "terminal.input",
    canType: () => true,
    rosterStatus: async () => ({
      rows: [
        {
          sessionKey: "claude:s-1",
          provider: "claude",
          displayName: "one",
          cwd: "/w",
          state: "waiting",
          provenance: "hook",
          tier: "hook",
          hasOpenRequest: true,
          lifecycleUpdatedAt: 5n,
          processStartFingerprint: "fp-1",
          version: 3n,
          readRevision: 11n,
        },
      ],
      readRevision: 11n,
      readAtMs: 1n,
    }),
    subscribeFleet: async (after) => {
      calls.subscribeFleet.push(Number(after));
      return { headRevision: 6n, replayComplete: true };
    },
    attentionList: async () => {
      calls.attentionList += 1;
      return [{ id: "att-2", sessionId: "s-1", kind: "ask_user_question", version: 9n, payload: { question: "which?", options: [], text: undefined, message: undefined }, degraded: false, createdAt: 4n }];
    },
    subscribeAttention: async () => [
      { id: "att-1", sessionId: "s-1", kind: "approval", version: 4n, payload: { question: "deploy?", options: [{ label: "yes", description: "" }], text: undefined, message: undefined }, degraded: false, createdAt: 7n },
    ],
    answer: async (_a, _b, version, opId) => {
      calls.answer.push({ version: Number(version), opId });
      refuse();
      return { opId, outcome: { tag: "Delivered", inner: { via: "tmux (x)" } }, ack: { status: "accepted", outcome: "created", receipt: "delivered" } };
    },
    sendPrompt: async (_s, _t, _l, opId) => {
      calls.sendPrompt.push({ opId });
      refuse();
      return { opId, messageId: "m1", ack: { status: "accepted" } };
    },
    interrupt: async (_s, version, fingerprint, opId) => {
      calls.interrupt.push({ version: Number(version), fingerprint, opId });
      refuse();
      return { opId, receiptStatus: "DELIVERED", ack: { status: "accepted" } };
    },
    transcriptPage: async (_s, afterOrder, limit) => {
      const after = afterOrder === undefined ? undefined : Number(afterOrder);
      calls.transcriptPage.push({ afterOrder: after, limit });
      const all = opts.transcript ?? [9];
      const from = after === undefined ? all.slice(-limit) : all.filter((o) => o > after).slice(0, limit);
      return { chunks: from.map(chunk), truncated: false };
    },
    subscribeTranscript: async () => 9n,
    terminalAttach: async () => ({ streamId: 7n, epoch: 2n, snapshotSeq: 100n, cols: 40, rows: 20, floor: { holder: { principal: "local", label: "desktop", streamId: 1n }, floorGen: 3n }, nativeClients: 1 }),
    terminalDetach: async () => undefined,
    terminalInput: async (_s, _d, floorGen, opId) => {
      calls.terminalInput.push(opId);
      return floorGen === 5 ? { tag: "Typed", inner: { opId, floorGen: 5n } } : { tag: "FloorDenied", inner: { holder: { principal: "local", label: "desktop", streamId: 1n }, floorGen: 3n } };
    },
    terminalFloor: async (_s, action, opId) => {
      calls.terminalFloor.push(opId);
      return action === "take" ? { tag: "Floor", inner: { opId, floor: { holder: { principal: "device:d1", label: "phone", streamId: 7n }, floorGen: 5n } } } : { tag: "FloorDenied", inner: { floorGen: 3n } };
    },
    terminalResize: async (_s, cols, rows) => ({ tag: "Applied", inner: { cols, rows } }),
    nextEvent: () => {
      const next = queue.shift();
      if (next) {
        if (next.tag === "Closed") closed = true;
        return Promise.resolve(next);
      }
      if (opts.pumpFails) return Promise.reject(new Error("binding gone"));
      if (closed) return Promise.resolve(localClose);
      return new Promise<Tagged>((resolve) => {
        waiter = resolve;
      });
    },
    close: () => {
      closed = true;
      queue.length = 0;
      waiter?.(localClose);
      waiter = undefined;
    },
    isClosed: () => closed,
    connectionLog: () => [],
  };
  return { host, calls };
}

function scriptedBindings(host: NativeMobileHost, opts: { connectError?: Tagged; dials?: { count: number }; slow?: boolean } = {}): Bindings {
  return {
    parseOffer: () => ({ hostId: "h1", endpoints: [{ carrier: "lan", url: "ws://x/peer" }], inviteId: "i", expiresAtMs: 9n }),
    pair: async () => ({ hostId: "h1", hostStaticPubkey: new Uint8Array(32), endpoints: [], deviceId: "d1", displayName: "phone", scope: "mobile+type", admin: false, expiresAtMs: 9n, pairedAtMs: 1n }),
    listPairings: () => [{ hostId: "h1", hostStaticPubkey: new Uint8Array(32), endpoints: [], deviceId: "d1", displayName: "phone", scope: "mobile+type", admin: false, expiresAtMs: 9n, pairedAtMs: 1n, repair: "revoked", notice: undefined }],
    forgetPairing: () => undefined,
    connectHost: async () => {
      if (opts.dials) opts.dials.count += 1;
      if (opts.slow) await new Promise((r) => setTimeout(r, 5));
      if (opts.connectError) throw opts.connectError;
      return host;
    },
    mintOpId: () => "0123456789abcdef0123456789abcdef",
    backoffDelayMs: () => 1000n,
    readConnectionLog: () => [{ tMs: 1n, event: "connect", detail: "{}" }],
    deviceKeyFingerprint: () => "ab".repeat(32),
    latchValues: () => CRATE_LATCH_VALUES,
  };
}

const flush = () => new Promise((r) => setTimeout(r, 0));

function wireOver(host: NativeMobileHost, bopts?: Parameters<typeof scriptedBindings>[1]) {
  return new NativeWire({ custodyDir: "/c", logDir: "/l", bindings: scriptedBindings(host, bopts) });
}

describe("native adapter", () => {
  test("the app's five latch strings are the crate's exported constants", () => {
    const b = scriptedBindings(scriptedHost([]).host);
    expect([...APP_REPAIR, ...APP_NOTICE].sort()).toEqual([...b.latchValues()].sort());
    expect(b.latchValues()).toHaveLength(5);
  });

  test("hosts carry the record's latch strings and reachability", async () => {
    const wire = wireOver(scriptedHost([]).host);
    const h = must((await wire.hosts())[0], "host row");
    expect(h.hostId).toBe("h1");
    expect(h.repair).toBe("revoked");
    expect(h.notice).toBeUndefined();
    expect(h.reachability).toBe("unreachable");
  });

  test("a refused connect is a PeerCloseError with the crate's code and verdict", async () => {
    const refused: Tagged = { tag: "Closed", inner: { code: 4403n, reason: "revoked", retryable: false } };
    const wire = wireOver(scriptedHost([]).host, { connectError: refused });
    await expect(wire.connect("h1")).rejects.toMatchObject({ kind: "closed", code: 4403, retryable: false });
    const lost = toPeerCloseError({ tag: "Closed", inner: { code: undefined, reason: "eof", retryable: true } });
    expect(isPeerCloseError(lost) && lost.retryable && lost.code === undefined).toBe(true);
    const changed = toPeerCloseError({ tag: "PeerChanged" });
    expect(changed.kind).toBe("peer_changed");
    expect(changed.retryable).toBe(false);
    const busy = toPeerCloseError({ tag: "Closed", inner: { code: 4429n, reason: "retry-after=7", retryable: true, retryAfterMs: 7000n } });
    expect(busy.retryAfterMs).toBe(7000);
  });

  test("concurrent connects share one dial: one socket, one promise", async () => {
    const dials = { count: 0 };
    const wire = wireOver(scriptedHost([]).host, { dials, slow: true });
    await Promise.all([wire.connect("h1"), wire.connect("h1"), wire.connect("h1")]);
    expect(dials.count).toBe(1);
    // Connected already: no dial either.
    await wire.connect("h1");
    expect(dials.count).toBe(1);
    // A dial that fails is not remembered: the next connect dials again.
    const failing = wireOver(scriptedHost([]).host, { dials: { count: 0 }, connectError: { tag: "Connect", inner: { message: "refused" } } });
    await expect(Promise.all([failing.connect("h1"), failing.connect("h1")])).rejects.toMatchObject({ kind: "connect", retryable: true });
    await expect(failing.connect("h1")).rejects.toMatchObject({ kind: "connect" });
  });

  test("events are pushed decoded, a raised attention as its real row, then the close with the crate's verdict", async () => {
    const events: Tagged[] = [
      { tag: "FleetRevision", inner: { event: { revision: 7n } } },
      { tag: "AttentionRaised", inner: { attentionId: "att-2", sessionId: "s-1", kind: "ask_user_question", createdAt: 4n } },
      { tag: "TranscriptChunk", inner: { chunk: { ingestOrder: 12n, eventId: "e12", sessionKey: "claude:s-1", eventType: "acp.user_message", role: "user", text: "go", payload: "{}", observedAt: 3n } } },
      { tag: "TerminalFrame", inner: { streamId: 7n, seq: 105n, frame: { tag: "Output", inner: { data: new Uint8Array([104, 105]).buffer } } } },
      { tag: "Closed", inner: { code: 4503n, reason: "draining", retryable: true } },
    ];
    const { host, calls } = scriptedHost(events);
    const wire = wireOver(host);
    const seen: WireEvent[] = [];
    wire.onEvent((ev) => seen.push(ev));
    await wire.connect("h1");
    for (let i = 0; i < 20 && seen.length < 5; i++) await flush();
    expect(seen.map((e) => e.kind)).toEqual(["fleet_revision", "attention_raised", "transcript_line", "terminal_frame", "closed"]);
    const raised = must(seen[1], "attention event");
    if (raised.kind !== "attention_raised") throw new Error("not a raise");
    expect(raised.row).toMatchObject({ id: "att-2", version: 9, payload: { question: "which?" } });
    expect(calls.attentionList).toBe(1);
    const line = must(seen[2], "transcript event");
    if (line.kind !== "transcript_line") throw new Error("not a line");
    expect(line.entry).toEqual({ seq: 12, role: "user", text: "go", atMs: 3 });
    const frame = must(seen[3], "frame event");
    if (frame.kind !== "terminal_frame" || frame.frame.kind !== "output") throw new Error(`not an output frame: ${JSON.stringify(frame)}`);
    expect(Array.from(frame.frame.data)).toEqual([104, 105]);
    const closed = must(seen[4], "close event");
    if (closed.kind !== "closed") throw new Error(`not a close: ${JSON.stringify(closed)}`);
    expect(closed.code).toBe(4503);
    expect(closed.retryable).toBe(true);
    expect(must((await wire.hosts())[0], "host row").reachability).toBe("unreachable");
  });

  test("a failed event pull closes the host with retryable false", async () => {
    const wire = wireOver(scriptedHost([], { pumpFails: true }).host);
    const seen: WireEvent[] = [];
    wire.onEvent((ev) => seen.push(ev));
    await wire.connect("h1");
    for (let i = 0; i < 10 && seen.length < 1; i++) await flush();
    expect(seen[0]).toMatchObject({ kind: "closed", hostId: "h1", retryable: false });
    expect(must((await wire.hosts())[0], "host row").reachability).toBe("unreachable");
  });

  test("the first fleet subscribe asks for the snapshot at the roster's revision", async () => {
    const { host, calls } = scriptedHost([]);
    const wire = wireOver(host);
    await wire.connect("h1");
    expect(await wire.subscribeFleet("h1")).toEqual({ revision: 6, replayState: "complete" });
    expect(await wire.subscribeFleet("h1", 4)).toEqual({ revision: 6, replayState: "complete" });
    expect(calls.subscribeFleet).toEqual([11, 4]);
  });

  test("a -32008 answers as the rejected outcome the app renders, -32009 as unknown, never a throw", async () => {
    const turned = scriptedHost([], { refuse: { code: -32008, reason: "turn_advanced" } });
    const wire = wireOver(turned.host);
    await wire.connect("h1");
    const reply = await wire.answer({ hostId: "h1", attentionId: "att-1", answer: "yes", version: 4, opId: "op-a" });
    expect(reply.outcome).toEqual({ kind: "rejected", reason: "turn_advanced" });
    expect(reply.ack).toEqual({ status: "rejected", reason: "turn_advanced" });
    expect(await wire.sendPrompt({ hostId: "h1", sessionKey: "claude:s-1", text: "hi", lifecycleUpdatedAt: 5, opId: "op-p" })).toEqual({ status: "rejected", reason: "turn_advanced" });
    expect(await wire.interrupt({ hostId: "h1", sessionKey: "claude:s-1", sessionIncarnation: "fp-1", version: 3, opId: "op-i" })).toEqual({ status: "rejected", reason: "turn_advanced" });
    expect(turned.calls.answer).toEqual([{ version: 4, opId: "op-a" }]);

    const unknown = wireOver(scriptedHost([], { refuse: { code: -32009, reason: "ledger_unavailable" } }).host);
    await unknown.connect("h1");
    expect((await unknown.answer({ hostId: "h1", attentionId: "att-1", answer: "yes", version: 4, opId: "op-a" })).outcome).toEqual({ kind: "unknown", reason: "ledger_unavailable" });

    const other = wireOver(scriptedHost([], { refuse: { code: -32602, reason: "bad params" } }).host);
    await other.connect("h1");
    await expect(other.sendPrompt({ hostId: "h1", sessionKey: "claude:s-1", text: "hi", lifecycleUpdatedAt: 5, opId: "op-p" })).rejects.toMatchObject({ kind: "rpc" });
  });

  test("interrupt fences on the version and incarnation of the row the user saw", async () => {
    const { host, calls } = scriptedHost([]);
    const wire = wireOver(host);
    await wire.connect("h1");
    const row = must((await wire.rosterStatus("h1"))[0], "roster row");
    expect(row).toMatchObject({ sessionKey: "claude:s-1", state: "waiting", provenance: "hook", tier: "hook", lifecycleUpdatedAt: 5, sessionIncarnation: "fp-1", version: 3 });
    // The row the user saw is older than the roster now; the fence carries the old version.
    await wire.interrupt({ hostId: "h1", sessionKey: row.sessionKey, sessionIncarnation: row.sessionIncarnation, version: 2, opId: "op-i" });
    expect(calls.interrupt).toEqual([{ version: 2, fingerprint: "fp-1", opId: "op-i" }]);
  });

  test("transcript pages come decoded by the crate, and beforeSeq fetches older entries", async () => {
    const transcript = Array.from({ length: 30 }, (_, i) => 100 + i * 3);
    const { host, calls } = scriptedHost([], { transcript });
    const wire = wireOver(host);
    await wire.connect("h1");
    const newest = await wire.transcriptPage("h1", "claude:s-1");
    expect(newest.at(-1)).toEqual({ seq: 187, role: "agent", text: "line 187", atMs: 2 });
    expect(must(calls.transcriptPage[0], "first page call").afterOrder).toBeUndefined();
    const older = await wire.transcriptPage("h1", "claude:s-1", 112);
    expect(older.map((e) => e.seq)).toEqual([100, 103, 106, 109]);
    expect(older.every((e) => e.seq < 112)).toBe(true);
    const call = must(calls.transcriptPage[1], "older page call");
    expect(call.afterOrder).toBeLessThan(112);
    expect(await wire.transcriptPage("h1", "claude:s-1", 100)).toEqual([]);
  });

  test("the app's own close reports retryable false, a floor refusal is a value, and terminal calls carry the app's op id", async () => {
    const { host, calls } = scriptedHost([]);
    const wire = wireOver(host);
    const seen: WireEvent[] = [];
    wire.onEvent((ev) => seen.push(ev));
    await wire.connect("h1");
    const attached = await wire.terminalAttach({ hostId: "h1", sessionKey: "claude:s-1", cols: 40, rows: 20, wantInput: true });
    expect(attached.floor.holder?.label).toBe("desktop");
    const denied = await wire.terminalInput({ hostId: "h1", streamId: 7, floorGen: 3, data: "ls\n", opId: "op-1" });
    expect(denied).toMatchObject({ kind: "floor_denied", floorGen: 3 });
    const taken = await wire.terminalFloor({ hostId: "h1", streamId: 7, action: "take", opId: "op-2" });
    expect(taken).toMatchObject({ floorGen: 5 });
    const typed = await wire.terminalInput({ hostId: "h1", streamId: 7, floorGen: 5, data: "ls\n", opId: "op-1" });
    expect(typed).toEqual({ floorGen: 5 });
    expect(calls.terminalInput).toEqual(["op-1", "op-1"]);
    expect(calls.terminalFloor).toEqual(["op-2"]);
    const att = must((await wire.subscribeAttention("h1"))[0], "attention row");
    expect(att.payload).toEqual({ question: "deploy?", options: ["yes"], text: undefined });
    const reply = await wire.answer({ hostId: "h1", attentionId: "att-1", answer: "yes", version: 4, opId: await wire.mintOpId() });
    expect(reply.outcome).toEqual({ kind: "delivered", via: "tmux (x)" });
    expect(reply.ack?.receipt).toBe("delivered");
    await wire.close("h1");
    for (let i = 0; i < 10 && !seen.some((e) => e.kind === "closed"); i++) await flush();
    const closed = must(seen.find((e) => e.kind === "closed"), "close event");
    if (closed.kind !== "closed") throw new Error("not a close");
    expect(closed.retryable).toBe(false);
  });

  test("an unknown crate event is dropped, not mis-rendered", () => {
    expect(toWireEvent("h1", { tag: "Hologram", inner: {} })).toBeUndefined();
    expect(toWireEvent("h1", { tag: "Lagged", inner: { dropped: 3n } })).toEqual({ kind: "lagged", hostId: "h1" });
  });
});
