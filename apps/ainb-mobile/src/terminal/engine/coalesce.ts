/**
 * Batch every write that lands inside one tick into a single flush, so a
 * flood of small frames costs one terminal write per animation frame instead
 * of one per packet. Pure: the scheduler is injected (rAF in the webview,
 * a fake in tests).
 */
export function makeCoalescer<T>(flush: (batch: T[]) => void, schedule: (cb: () => void) => void) {
  let pending: T[] = [];
  let armed = false;
  return {
    push(item: T) {
      pending.push(item);
      if (armed) return;
      armed = true;
      schedule(() => {
        armed = false;
        const batch = pending;
        pending = [];
        flush(batch);
      });
    },
    /** Flush now, for a resize or a snapshot boundary. */
    drain() {
      if (pending.length === 0) return;
      const batch = pending;
      pending = [];
      flush(batch);
    },
  };
}

/** Join byte chunks into one buffer for a single `write`. */
export function concat(chunks: Uint8Array[]): Uint8Array {
  if (chunks.length === 1) return chunks[0]!;
  let n = 0;
  for (const c of chunks) n += c.length;
  const out = new Uint8Array(n);
  let at = 0;
  for (const c of chunks) {
    out.set(c, at);
    at += c.length;
  }
  return out;
}
