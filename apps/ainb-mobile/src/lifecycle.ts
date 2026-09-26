// The lifecycle owns every paired host's socket (DECISIONS DV3, C8):
//
// - at start, after a pairing and on every `active`: connect each paired
//   host that is not latched, subscribe the fleet stream from the last
//   revision seen (a snapshot on the first connect), reconcile the attention
//   inbox, then run the `afterForeground` hooks (the terminal re-attaches);
// - on `background` (never on iOS `inactive`): run the `beforeBackground`
//   hooks, each bounded, then close every socket, whatever the hooks did;
// - transitions run one after another, and a connect that finishes after a
//   background began closes itself again, so no socket outlives the
//   foreground;
// - a retryable close (1013, 4429, 4503, or no code at all) redials with
//   jittered exponential backoff, 1 s to 60 s (C-R1-10); latching and
//   protocol closes do not redial.

import { useEffect } from "react";
import { AppState, type AppStateStatus } from "react-native";

import { reconcile } from "./attention/store";
import { PeerCloseError, type FleetCursor, type HostId, type WireClient } from "./wire/types";

interface Live {
  cursor?: number;
  replay?: FleetCursor["replayState"];
  connected: boolean;
  attempts: number;
  /** The host's `retry-after`, kept for every redial of this outage. */
  retryAfter?: number;
  timer?: ReturnType<typeof setTimeout>;
}

const live = new Map<HostId, Live>();
const inflight = new Map<HostId, Promise<FleetCursor>>();
const onBackground = new Set<() => Promise<void> | void>();
const onForeground = new Set<() => Promise<void> | void>();
let chain: Promise<void> = Promise.resolve();
let generation = 0;
let foreground = true;

/** Close codes worth redialing through (T9). Undefined is a plain network drop. */
const RETRYABLE = new Set([1013, 4429, 4503]);

/** Whether a dial failure may be redialled: a network error yes, a peer close only for the three retryable codes. */
function mayRedial(e: unknown): boolean {
  if (e instanceof PeerCloseError) return e.code === undefined || RETRYABLE.has(e.code);
  return true;
}
const HOOK_BUDGET_MS = 500;
const RETRY_AFTER_PREFIX = "retry-after=";

/** Something that must let go before the socket does (the terminal stream). */
export function beforeBackground(cb: () => Promise<void> | void) {
  onBackground.add(cb);
  return () => onBackground.delete(cb);
}

/** Something to bring back once the hosts are reconnected (the terminal stream). */
export function afterForeground(cb: () => Promise<void> | void) {
  onForeground.add(cb);
  return () => onForeground.delete(cb);
}

/** The `<s>` of a `retry-after=<s>` close reason (peer_close.rs), if present. */
export function retryAfterSecs(reason: string | undefined): number | undefined {
  if (!reason) return undefined;
  const at = reason.indexOf(RETRY_AFTER_PREFIX);
  if (at < 0) return undefined;
  const n = Number.parseInt(reason.slice(at + RETRY_AFTER_PREFIX.length), 10);
  return Number.isFinite(n) && n >= 0 ? n : undefined;
}

function entry(hostId: HostId): Live {
  let l = live.get(hostId);
  if (!l) {
    l = { connected: false, attempts: 0 };
    live.set(hostId, l);
  }
  return l;
}

/**
 * Connect a host once. A second call while connected or in flight returns the
 * same result instead of dialling again.
 */
export function connectHost(wire: WireClient, hostId: HostId): Promise<FleetCursor> {
  const l = entry(hostId);
  if (l.connected) return Promise.resolve({ revision: l.cursor ?? 0, replayState: "complete" });
  const pending = inflight.get(hostId);
  if (pending) return pending;
  const myGeneration = generation;
  const p = (async () => {
    await wire.connect(hostId);
    // Tracked from this instant: whatever happens next, the socket is ours to close.
    l.connected = true;
    try {
      const cursor = await wire.subscribeFleet(hostId, l.cursor);
      l.cursor = cursor.revision;
      l.replay = cursor.replayState;
      l.attempts = 0;
      l.retryAfter = undefined;
      await reconcile(wire, hostId);
      if (!foreground || myGeneration !== generation) {
        // A background began while we were dialling: DV3 says no socket stays open.
        l.connected = false;
        await wire.close(hostId).catch(() => undefined);
      }
      return cursor;
    } catch (e) {
      // A half-set-up session is not a connection: close it now and let the caller see the failure.
      l.connected = false;
      await wire.close(hostId).catch(() => undefined);
      throw e;
    }
  })().finally(() => inflight.delete(hostId));
  inflight.set(hostId, p);
  return p;
}

/**
 * Redial after a delay, and keep redialling on failure with growing backoff,
 * until the dial succeeds, the app backgrounds, or a non-retryable close
 * arrives (which clears the timer through `onClosed`).
 */
function scheduleRedial(wire: WireClient, hostId: HostId) {
  const l = entry(hostId);
  if (l.timer) clearTimeout(l.timer);
  l.timer = setTimeout(() => {
    l.timer = undefined;
    if (!foreground) return;
    connectHost(wire, hostId).catch((e: unknown) => {
      if (!mayRedial(e)) return; // 4401, 4403, 4409 or an unknown code: the row shows the latch, no more dials
      if (e instanceof PeerCloseError) l.retryAfter = retryAfterSecs(e.reason) ?? l.retryAfter;
      l.attempts += 1;
      scheduleRedial(wire, hostId); // the host's retry-after still applies
    });
  }, wire.backoffDelayMs(l.attempts, l.retryAfter));
}

/** Connect every paired host that is not latched (revoked, identity, parked); a failed dial enters the redial loop. */
export async function connectAll(wire: WireClient) {
  const hosts = await wire.hosts().catch(() => []);
  await Promise.allSettled(
    hosts
      .filter((h) => !h.repair && !h.notice)
      .map((h) =>
        connectHost(wire, h.hostId).catch((e: unknown) => {
          if (foreground && mayRedial(e)) scheduleRedial(wire, h.hostId);
        }),
      ),
  );
}

async function closeAll(wire: WireClient) {
  for (const [hostId, l] of live) {
    if (l.timer) clearTimeout(l.timer);
    l.timer = undefined;
    if (!l.connected) continue;
    l.connected = false;
    await wire.close(hostId).catch(() => undefined);
  }
}

function bounded(run: () => Promise<void> | void): Promise<void> {
  return Promise.race([
    Promise.resolve().then(run),
    new Promise<void>((resolve) => setTimeout(resolve, HOOK_BUDGET_MS)),
  ]);
}

async function transition(wire: WireClient, next: AppStateStatus) {
  if (next === "active") {
    foreground = true;
    generation += 1;
    await connectAll(wire);
    await Promise.allSettled([...onForeground].map(bounded));
  } else if (next === "background") {
    foreground = false;
    generation += 1;
    try {
      await Promise.allSettled([...onBackground].map(bounded));
    } finally {
      await closeAll(wire);
    }
  }
  // `inactive` (Control Center, a permission prompt, the app switcher),
  // `unknown` and `extension` change nothing.
}

/** Serialised: a transition never overlaps the one before it. */
export function onAppState(wire: WireClient, next: AppStateStatus): Promise<void> {
  chain = chain.then(() => transition(wire, next)).catch(() => undefined);
  return chain;
}

export function noteRevision(hostId: HostId, revision: number) {
  const l = live.get(hostId);
  if (l && revision > (l.cursor ?? -1)) l.cursor = revision;
}

/** The socket went away while foregrounded: redial when the code allows it. */
function onClosed(wire: WireClient, hostId: HostId, code: number | undefined, reason: string | undefined) {
  const l = entry(hostId);
  l.connected = false;
  if (l.timer) clearTimeout(l.timer);
  l.timer = undefined;
  if (!foreground) return;
  if (code !== undefined && !RETRYABLE.has(code)) return;
  l.retryAfter = retryAfterSecs(reason) ?? l.retryAfter;
  scheduleRedial(wire, hostId); // attempt 0 waits 1 s (or retry-after); each failure doubles it
}

/** A lag or a resync: subscribe again, from the cursor or from a snapshot. */
async function resubscribe(wire: WireClient, hostId: HostId, fromSnapshot: boolean) {
  const l = entry(hostId);
  if (!l.connected) return;
  const cursor = await wire.subscribeFleet(hostId, fromSnapshot ? undefined : l.cursor).catch(() => undefined);
  if (!cursor) return;
  l.cursor = cursor.revision;
  l.replay = cursor.replayState;
  await reconcile(wire, hostId).catch(() => undefined);
}

export function liveHosts(): ReadonlyMap<HostId, Readonly<Live>> {
  return live;
}

export function resetLifecycle() {
  for (const l of live.values()) if (l.timer) clearTimeout(l.timer);
  live.clear();
  inflight.clear();
  onBackground.clear();
  onForeground.clear();
  chain = Promise.resolve();
  generation = 0;
  foreground = true;
}

export function useLifecycle(wire: WireClient) {
  useEffect(() => {
    void connectAll(wire);
    const sub = AppState.addEventListener("change", (next) => void onAppState(wire, next));
    const off = wire.onEvent((ev) => {
      if (ev.kind === "fleet_revision") noteRevision(ev.hostId, ev.revision);
      else if (ev.kind === "closed") onClosed(wire, ev.hostId, ev.code, ev.reason);
      else if (ev.kind === "lagged") void resubscribe(wire, ev.hostId, false);
      else if (ev.kind === "fleet_resync_required") void resubscribe(wire, ev.hostId, true);
    });
    return () => {
      sub.remove();
      off();
    };
  }, [wire]);
}
