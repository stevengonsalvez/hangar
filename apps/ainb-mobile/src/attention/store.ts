// Open attention rows across every host, deduped by id, as a tiny external
// store so the banner, the sheet and the lifecycle hook (M1-06) share one
// truth without a provider.

import { useSyncExternalStore } from "react";

import type { AttentionRow, WireClient } from "../wire/types";

interface State {
  rows: AttentionRow[];
  /** Ids ever shown as a banner; a reconnect never re-shows one. */
  seen: Set<string>;
}

let state: State = { rows: [], seen: new Set() };
const listeners = new Set<() => void>();

function set(next: State) {
  state = next;
  for (const l of listeners) l();
}

export function raise(row: AttentionRow) {
  if (state.seen.has(row.id)) return;
  const seen = new Set(state.seen).add(row.id);
  set({ rows: [...state.rows, row], seen });
}

export function retire(attentionId: string) {
  if (!state.rows.some((r) => r.id === attentionId)) return;
  set({ ...state, rows: state.rows.filter((r) => r.id !== attentionId) });
}

/** Pull the open rows for a host and raise the ones we have not shown. */
export async function reconcile(wire: WireClient, hostId: string) {
  const rows = await wire.subscribeAttention(hostId);
  const open = new Set(rows.map((r) => r.id));
  for (const r of state.rows) if (r.hostId === hostId && !open.has(r.id)) retire(r.id);
  for (const r of rows) raise(r);
}

/** Tests start from nothing. */
export function reset() {
  set({ rows: [], seen: new Set() });
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
