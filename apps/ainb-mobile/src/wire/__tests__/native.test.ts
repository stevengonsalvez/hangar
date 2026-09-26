// The native adapter against a scripted binding: every mapping the screens
// depend on, and the five latch strings compared to the crate's exported
// constants (`latchValues()`), so the app keeps no copy of their meaning.

import type { Bindings, NativeMobileHost, Tagged } from "../native";
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

function scriptedHost(events: Tagged[], opts: { closed?: boolean } = {}): NativeMobileHost {
  const queue = [...events];
  let closed = opts.closed ?? false;
  // With no scripted event left, `nextEvent` waits like the crate's pump
  // until `close()` produces the local close.
  let waiter: ((t: Tagged) => void) | undefined;
  const localClose: Tagged = { tag: "Closed", inner: { code: undefined, reason: "closed by client", retryable: false } };
  return {
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
    subscribeFleet: async () => ({ headRevision: 6n, replayComplete: true }),
    subscribeAttention: async () => [
      { id: "att-1", sessionId: "s-1", kind: "approval", version: 4n, payload: { question: "deploy?", options: [{ label: "yes", description: "" }], text: undefined, message: undefined }, degraded: false, createdAt: 7n },
    ],
    answer: async (_a, _b, _c, opId) => ({ opId, outcome: { tag: "Delivered", inner: { via: "tmux (x)" } }, ack: { status: "accepted", outcome: "created", receipt: "delivered" } }),
    sendPrompt: async (_s, _t, _l, opId) => ({ opId, messageId: "m1", ack: { status: "accepted" } }),
    interrupt: async (_s, _v, _f, opId) => ({ opId, receiptStatus: "DELIVERED", ack: { status: "accepted" } }),
    transcriptPage: async () => ({ chunks: [{ ingestOrder: 9n, eventId: "e9", sessionKey: "claude:s-1", eventType: "acp.agent_message", payload: JSON.stringify({ text: "hi" }), observedAt: 2n }], truncated: false }),
    subscribeTranscript: async () => 9n,
    terminalAttach: async () => ({ streamId: 7n, epoch: 2n, snapshotSeq: 100n, cols: 40, rows: 20, floor: { holder: { principal: "local", label: "desktop", streamId: 1n }, floorGen: 3n }, nativeClients: 1 }),
    terminalDetach: async () => undefined,
    terminalInput: async (_s, _d, floorGen) =>
      floorGen === 5 ? { tag: "Typed", inner: { opId: "x", floorGen: 5n } } : { tag: "FloorDenied", inner: { holder: { principal: "local", label: "desktop", streamId: 1n }, floorGen: 3n } },
    terminalFloor: async (_s, action) =>
      action === "take" ? { tag: "Floor", inner: { opId: "x", floor: { holder: { principal: "device:d1", label: "phone", streamId: 7n }, floorGen: 5n } } } : { tag: "FloorDenied", inner: { floorGen: 3n } },
    terminalResize: async (_s, cols, rows) => ({ tag: "Applied", inner: { cols, rows } }),
    nextEvent: () => {
      const next = queue.shift();
      if (next) {
        if (next.tag === "Closed") closed = true;
        return Promise.resolve(next);
      }
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
}

function scriptedBindings(host: NativeMobileHost, connectError?: Tagged): Bindings {
  return {
    parseOffer: () => ({ hostId: "h1", endpoints: [{ carrier: "lan", url: "ws://x/peer" }], inviteId: "i", expiresAtMs: 9n }),
    pair: async () => ({ hostId: "h1", hostStaticPubkey: new Uint8Array(32), endpoints: [], deviceId: "d1", displayName: "phone", scope: "mobile+type", admin: false, expiresAtMs: 9n, pairedAtMs: 1n }),
    listPairings: () => [{ hostId: "h1", hostStaticPubkey: new Uint8Array(32), endpoints: [], deviceId: "d1", displayName: "phone", scope: "mobile+type", admin: false, expiresAtMs: 9n, pairedAtMs: 1n, repair: "revoked", notice: undefined }],
    forgetPairing: () => undefined,
    connectHost: async () => {
      if (connectError) throw connectError;
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

describe("native adapter", () => {
  test("the app's five latch strings are the crate's exported constants", () => {
    const b = scriptedBindings(scriptedHost([]));
    expect([...APP_REPAIR, ...APP_NOTICE].sort()).toEqual([...b.latchValues()].sort());
    expect(b.latchValues()).toHaveLength(5);
  });

  test("hosts carry the record's latch strings and reachability", async () => {
    const wire = new NativeWire({ custodyDir: "/c", logDir: "/l", bindings: scriptedBindings(scriptedHost([])) });
    const h = must((await wire.hosts())[0], "host row");
    expect(h.hostId).toBe("h1");
    expect(h.repair).toBe("revoked");
    expect(h.notice).toBeUndefined();
    expect(h.reachability).toBe("unreachable");
  });

  test("a refused connect is a PeerCloseError with the crate's code and verdict", async () => {
    const refused: Tagged = { tag: "Closed", inner: { code: 4403n, reason: "revoked", retryable: false } };
    const wire = new NativeWire({ custodyDir: "/c", logDir: "/l", bindings: scriptedBindings(scriptedHost([]), refused) });
    await expect(wire.connect("h1")).rejects.toMatchObject({ kind: "closed", code: 4403, retryable: false });
    const lost = toPeerCloseError({ tag: "Closed", inner: { code: undefined, reason: "eof", retryable: true } });
    expect(isPeerCloseError(lost) && lost.retryable && lost.code === undefined).toBe(true);
    const changed = toPeerCloseError({ tag: "PeerChanged" });
    expect(changed.kind).toBe("peer_changed");
    expect(changed.retryable).toBe(false);
    const busy = toPeerCloseError({ tag: "Closed", inner: { code: 4429n, reason: "retry-after=7", retryable: true, retryAfterMs: 7000n } });
    expect(busy.retryAfterMs).toBe(7000);
  });

  test("events are pushed decoded, then the close with the crate's verdict", async () => {
    const events: Tagged[] = [
      { tag: "FleetRevision", inner: { event: { revision: 7n } } },
      { tag: "AttentionRaised", inner: { attentionId: "att-2", sessionId: "s-1", kind: "ask_user_question", createdAt: 4n } },
      { tag: "TerminalFrame", inner: { streamId: 7n, seq: 105n, frame: { tag: "Output", inner: { data: new Uint8Array([104, 105]).buffer } } } },
      { tag: "Closed", inner: { code: 4503n, reason: "draining", retryable: true } },
    ];
    const wire = new NativeWire({ custodyDir: "/c", logDir: "/l", bindings: scriptedBindings(scriptedHost(events)) });
    const seen: WireEvent[] = [];
    wire.onEvent((ev) => seen.push(ev));
    await wire.connect("h1");
    for (let i = 0; i < 10 && seen.length < 4; i++) await flush();
    expect(seen.map((e) => e.kind)).toEqual(["fleet_revision", "attention_raised", "terminal_frame", "closed"]);
    const frame = must(seen[2], "frame event");
    if (frame.kind !== "terminal_frame" || frame.frame.kind !== "output") throw new Error(`not an output frame: ${JSON.stringify(frame)}`);
    expect(Array.from(frame.frame.data)).toEqual([104, 105]);
    const closed = must(seen[3], "close event");
    if (closed.kind !== "closed") throw new Error(`not a close: ${JSON.stringify(closed)}`);
    expect(closed.code).toBe(4503);
    expect(closed.retryable).toBe(true);
    expect(must((await wire.hosts())[0], "host row").reachability).toBe("unreachable");
  });

  test("the app's own close reports retryable false, and a floor refusal is a value", async () => {
    const host = scriptedHost([]);
    const wire = new NativeWire({ custodyDir: "/c", logDir: "/l", bindings: scriptedBindings(host) });
    const seen: WireEvent[] = [];
    wire.onEvent((ev) => seen.push(ev));
    await wire.connect("h1");
    const attached = await wire.terminalAttach({ hostId: "h1", sessionKey: "claude:s-1", cols: 40, rows: 20, wantInput: true });
    expect(attached.floor.holder?.label).toBe("desktop");
    const denied = await wire.terminalInput({ hostId: "h1", streamId: 7, floorGen: 3, data: "ls\n" });
    expect(denied).toMatchObject({ kind: "floor_denied", floorGen: 3 });
    const taken = await wire.terminalFloor({ hostId: "h1", streamId: 7, action: "take" });
    expect(taken).toMatchObject({ floorGen: 5 });
    const typed = await wire.terminalInput({ hostId: "h1", streamId: 7, floorGen: 5, data: "ls\n" });
    expect(typed).toEqual({ floorGen: 5 });
    const rows = await wire.rosterStatus("h1");
    expect(must(rows[0], "roster row")).toMatchObject({ sessionKey: "claude:s-1", state: "waiting", provenance: "hook", tier: "hook", lifecycleUpdatedAt: 5, sessionIncarnation: "fp-1" });
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
