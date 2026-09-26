// Open attention rows across every host as a tiny external store, so the
// banner, the sheet and the lifecycle share one truth without a provider.
//
// Three facts live here, each with its own rule:
// - `rows`: what is OPEN, from `reconcile` (the daemon's list) plus live
//   events. Rows leave only when answered (delivered, already answered, or an
//   `attention_answered` event). An ambiguous or no-target outcome keeps the
//   row: the question is still open on the daemon.
// - `notified`: which rows were already announced, so a reconnect never
//   re-announces one. It gates the announcement only, never the list.
// - `sent`: the one answer (and op id) that ever left the phone for a row.
//   A retry may resend only that answer under that id (D18 idempotency).

import { useSyncExternalStore } from "react";

import type { AttentionRow, HostId, WireClient } from "../wire/types";

export interface SentAnswer {
  opId: string;
  answer: string;
}

interface State {
  rows: AttentionRow[];
  notified: Set<string>;
  sent: Map<string, SentAnswer>;
}

export const rowKey = (hostId: HostId, id: string) => `${hostId}\u0000${id}`;

let state: State = { rows: [], notified: new Set(), sent: new Map() };
const listeners = new Set<() => void>();

function set(next: State) {
  state = next;
  for (const l of listeners) l();
}

/** Add an open row. Returns true the first time this row is announced. */
export function raise(row: AttentionRow): boolean {
  const key = rowKey(row.hostId, row.id);
  const fresh = !state.notified.has(key);
  const present = state.rows.some((r) => rowKey(r.hostId, r.id) === key);
  if (!fresh && present) return false;
  const notified = fresh ? new Set(state.notified).add(key) : state.notified;
  set({ ...state, rows: present ? state.rows : [...state.rows, row], notified });
  return fresh;
}

/** The row was answered (by us or elsewhere): it leaves the open list. */
export function retire(hostId: HostId, attentionId: string) {
  const key = rowKey(hostId, attentionId);
  if (!state.rows.some((r) => rowKey(r.hostId, r.id) === key)) return;
  set({ ...state, rows: state.rows.filter((r) => rowKey(r.hostId, r.id) !== key) });
}

/** Pull the open rows for a host: add the missing, drop the closed. */
export async function reconcile(wire: WireClient, hostId: HostId) {
  const rows = await wire.subscribeAttention(hostId);
  const open = new Set(rows.map((r) => r.id));
  for (const r of state.rows) if (r.hostId === hostId && !open.has(r.id)) retire(hostId, r.id);
  for (const r of rows) raise(r);
}

export function sentFor(hostId: HostId, attentionId: string): SentAnswer | undefined {
  return state.sent.get(rowKey(hostId, attentionId));
}

/** Pin the one answer that may ever go out for this row. */
export function markSent(hostId: HostId, attentionId: string, sent: SentAnswer) {
  const next = new Map(state.sent).set(rowKey(hostId, attentionId), sent);
  set({ ...state, sent: next });
}

/** Tests start from nothing. */
export function reset() {
  set({ rows: [], notified: new Set(), sent: new Map() });
}

export function useAttentionRows(): AttentionRow[] {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => state.rows,
  );
}
