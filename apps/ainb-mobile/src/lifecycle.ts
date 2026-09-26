// Foreground / background rule (DECISIONS DV3, C8): on leaving the foreground
// the phone detaches its terminal, closes every host socket and stops its
// timers; on `active` it reconnects each host, replays the fleet stream from
// the last revision it saw, and reconciles the attention inbox so a banner
// shows once per id.

import { useEffect } from "react";
import { AppState, type AppStateStatus } from "react-native";

import { reconcile } from "./attention/store";
import type { FleetCursor, HostId, WireClient } from "./wire/types";

interface Live {
  cursor: number;
  replay?: FleetCursor["replayState"];
}

const live = new Map<HostId, Live>();
const onBackground = new Set<() => Promise<void> | void>();
const onForeground = new Set<() => Promise<void> | void>();

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

/** Connect a host and remember it, so the lifecycle can drop and restore it. */
export async function connectHost(wire: WireClient, hostId: HostId) {
  await wire.connect(hostId);
  const prev = live.get(hostId);
  const cursor = await wire.subscribeFleet(hostId, prev?.cursor ?? 0);
  live.set(hostId, { cursor: cursor.revision, replay: cursor.replayState });
  await reconcile(wire, hostId);
  return cursor;
}

export function noteRevision(hostId: HostId, revision: number) {
  const l = live.get(hostId);
  if (l && revision > l.cursor) l.cursor = revision;
}

export function liveHosts(): ReadonlyMap<HostId, Live> {
  return live;
}

export async function onAppState(wire: WireClient, next: AppStateStatus) {
  if (next === "active") {
    for (const hostId of [...live.keys()]) await connectHost(wire, hostId).catch(() => undefined);
    for (const cb of onForeground) await cb();
  } else {
    for (const cb of onBackground) await cb();
    for (const hostId of live.keys()) await wire.close(hostId).catch(() => undefined);
  }
}

export function resetLifecycle() {
  live.clear();
  onBackground.clear();
  onForeground.clear();
}

export function useLifecycle(wire: WireClient) {
  useEffect(() => {
    const sub = AppState.addEventListener("change", (next) => void onAppState(wire, next));
    const off = wire.onEvent((ev) => {
      if (ev.kind === "fleet_revision") noteRevision(ev.hostId, ev.revision);
    });
    return () => {
      sub.remove();
      off();
    };
  }, [wire]);
}
