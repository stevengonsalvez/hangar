// One session's terminal over the wire (M1-13): attach with the phone's
// geometry, feed the sink from frames, arbitrate the floor, detach before
// background and re-attach after foreground.
import { useCallback, useEffect, useRef, useState } from "react";

import { afterForeground, beforeBackground } from "../lifecycle";
import { useWire, useWireEvents } from "../wire/context";
import type { FloorHolder, FloorState, HostId, HostInfo, SessionKey, TerminalFrame } from "../wire/types";
import type { TerminalSink } from "./TerminalView";

export interface TerminalState {
  streamId?: number;
  /** Scope is mobile+type and both terminal capabilities are advertised. */
  canType: boolean;
  /** The user switched typing on; the phone holds or wants the floor. */
  typing: boolean;
  floor: FloorState;
  nativeClients: number;
  /** Set when input was refused, by the daemon or locally: who holds the floor. */
  denied?: FloorHolder;
  closed?: string;
  cols?: number;
  rows?: number;
}

/** Keystrokes inside one frame go out as one receipt-tier mutation (M1-plan R8). */
const INPUT_BATCH_MS = 16;
/** Early frames held before the attach reply: one snapshot window's worth (T15 chunks are 48 KiB). */
const EARLY_BUFFER_BYTES = 256 * 1024;

function frameBytes(frame: TerminalFrame): number {
  return frame.kind === "snapshot_chunk" || frame.kind === "output" ? frame.data.length : 0;
}

export function useTerminal(hostId: HostId | undefined, sessionKey: SessionKey | undefined) {
  const wire = useWire();
  const sink = useRef<TerminalSink | undefined>(undefined);
  const stream = useRef<number | undefined>(undefined);
  /** The last geometry the webview reported; sent on attach and on every floor grant. */
  const lastFit = useRef<{ cols: number; rows: number } | undefined>(undefined);
  const snapshotSeq = useRef(0);
  const pendingInput = useRef<string>("");
  /**
   * Frames for a stream we are attaching to but have not recorded yet (they
   * can beat the attach reply), capped by payload bytes. Dropping the oldest
   * would lose the snapshot head, so an overflow discards the lot and asks for
   * a fresh snapshot by re-attaching once the pending attach settles.
   */
  const early = useRef<{ frames: { streamId: number; seq: number; frame: TerminalFrame }[]; bytes: number; overflowed: boolean }>({
    frames: [],
    bytes: 0,
    overflowed: false,
  });
  const inputTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const [state, setState] = useState<TerminalState>({ canType: false, typing: false, floor: { floorGen: 0 }, nativeClients: 0 });
  const stateRef = useRef(state);
  stateRef.current = state;
  const patch = (p: Partial<TerminalState>) => setState((s) => ({ ...s, ...p }));

  const mine = (floor: FloorState = stateRef.current.floor) => floor.holder !== undefined && floor.holder.streamId === stream.current;

  const resizeIfHolder = useCallback(
    async (floor: FloorState) => {
      const id = stream.current;
      const fit = lastFit.current;
      if (id === undefined || !hostId || !fit || floor.holder?.streamId !== id) return;
      await wire.terminalResize({ hostId, streamId: id, cols: fit.cols, rows: fit.rows }).catch(() => undefined);
    },
    [wire, hostId],
  );

  const detach = useCallback(async () => {
    const id = stream.current;
    stream.current = undefined;
    if (id !== undefined && hostId) await wire.terminalDetach(hostId, id).catch(() => undefined);
    setState((s) => ({ ...s, streamId: undefined }));
  }, [wire, hostId]);

  const onFrame = useCallback(
    (seq: number, frame: TerminalFrame) => {
      const s = sink.current;
      switch (frame.kind) {
        case "snapshot_start":
          s?.clear();
          snapshotSeq.current = seq;
          patch({ cols: frame.cols, rows: frame.rows });
          break;
        case "snapshot_chunk":
          s?.write(frame.data);
          break;
        case "output":
          // T16: output older than the snapshot we painted is already on screen.
          if (seq >= snapshotSeq.current) s?.write(frame.data);
          break;
        case "resize":
          patch({ cols: frame.cols, rows: frame.rows });
          break;
        case "data_gap":
          s?.clear(); // the daemon follows every gap with a fresh snapshot
          break;
        case "floor": {
          const floor = { holder: frame.holder, floorGen: frame.floorGen };
          patch({ floor, denied: undefined });
          void resizeIfHolder(floor);
          break;
        }
        case "presence":
          patch({ nativeClients: frame.nativeClients });
          break;
        case "closed":
          patch({ closed: frame.reason, streamId: undefined });
          stream.current = undefined;
          break;
        default:
          break;
      }
    },
    [resizeIfHolder],
  );

  const resetEarly = () => {
    early.current = { frames: [], bytes: 0, overflowed: false };
  };
  /** One fresh snapshot per overflow; a second overflow in a row is reported, not retried forever. */
  const overflowRetried = useRef(false);

  const attach = useCallback(async (): Promise<void> => {
    if (!hostId || !sessionKey || stream.current !== undefined) return;
    resetEarly();
    let at;
    try {
      const info: HostInfo = await wire.hostInfo(hostId).catch(() => ({ capabilities: [] }));
      const canType =
        info.scope?.base === "mobile+type" && info.capabilities.includes("terminal.stream") && info.capabilities.includes("terminal.input");
      const wantInput = canType && stateRef.current.typing;
      at = await wire.terminalAttach({ hostId, sessionKey, ...lastFit.current, wantInput });
      stream.current = at.streamId;
      snapshotSeq.current = at.snapshotSeq;
      patch({ streamId: at.streamId, canType, floor: at.floor, nativeClients: at.nativeClients, cols: at.cols, rows: at.rows, closed: undefined, denied: undefined });
    } catch (e) {
      resetEarly(); // nothing held for a stream that never came
      throw e;
    }
    const held = early.current;
    resetEarly();
    if (held.overflowed) {
      // The head of the snapshot is gone: start over with a fresh one, once.
      await detach();
      if (overflowRetried.current) {
        patch({ closed: "snapshot too large for the phone, reopen the terminal" });
        return;
      }
      overflowRetried.current = true;
      return attach();
    }
    overflowRetried.current = false;
    for (const q of held.frames) if (q.streamId === at.streamId) onFrame(q.seq, q.frame);
    await resizeIfHolder(at.floor);
  }, [wire, hostId, sessionKey, resizeIfHolder, detach]);

  useEffect(() => {
    void attach().catch((e: unknown) => patch({ closed: String(e instanceof Error ? e.message : e) }));
    const offB = beforeBackground(detach);
    const offF = afterForeground(() => attach().catch(() => undefined));
    return () => {
      offB();
      offF();
      void detach();
    };
  }, [attach, detach]);

  useWireEvents(
    useCallback(
      (ev) => {
        if (ev.kind !== "terminal_frame" || ev.hostId !== hostId) return;
        if (ev.streamId === stream.current) onFrame(ev.seq, ev.frame);
        else if (stream.current === undefined) {
          // Attach in flight: keep the frame until the reply names our stream.
          const e = early.current;
          if (e.overflowed) return;
          e.bytes += frameBytes(ev.frame);
          if (e.bytes > EARLY_BUFFER_BYTES) {
            e.frames = [];
            e.overflowed = true; // the attach continuation re-attaches for a fresh snapshot
            return;
          }
          e.frames.push({ streamId: ev.streamId, seq: ev.seq, frame: ev.frame });
        }
      },
      [hostId, onFrame],
    ),
  );

  const flushInput = useCallback(async () => {
    inputTimer.current = undefined;
    const data = pendingInput.current;
    pendingInput.current = "";
    const id = stream.current;
    if (!data || id === undefined || !hostId) return;
    try {
      const r = await wire.terminalInput({ hostId, streamId: id, floorGen: stateRef.current.floor.floorGen, data });
      if ("kind" in r) patch({ denied: r.holder, floor: { holder: r.holder, floorGen: r.floorGen } });
      else patch({ denied: undefined });
    } catch (e) {
      patch({ closed: String(e instanceof Error ? e.message : e) });
    }
  }, [wire, hostId]);

  /** Keys go out only while this stream holds the floor; otherwise the denial shows locally. */
  const input = useCallback(
    (data: string) => {
      const cur = stateRef.current;
      if (stream.current === undefined || !cur.typing) return;
      if (!mine(cur.floor)) {
        patch({ denied: cur.floor.holder ?? { principal: "", label: "nobody", streamId: 0 } });
        return;
      }
      pendingInput.current += data;
      inputTimer.current ??= setTimeout(() => void flushInput(), INPUT_BATCH_MS);
    },
    [flushInput],
  );

  const floor = useCallback(
    async (action: "acquire" | "release" | "take") => {
      const id = stream.current;
      if (id === undefined || !hostId) return;
      try {
        const r = await wire.terminalFloor({ hostId, streamId: id, action });
        if ("kind" in r) patch({ denied: r.holder, floor: { holder: r.holder, floorGen: r.floorGen } });
        else {
          patch({ floor: r, denied: undefined });
          await resizeIfHolder(r);
        }
      } catch (e) {
        patch({ closed: String(e instanceof Error ? e.message : e) });
      }
    },
    [wire, hostId, resizeIfHolder],
  );

  const setTyping = useCallback(
    async (on: boolean) => {
      patch({ typing: on, denied: undefined });
      await floor(on ? "acquire" : "release");
    },
    [floor],
  );

  const fit = useCallback(
    (cols: number, rows: number) => {
      lastFit.current = { cols, rows };
      void resizeIfHolder(stateRef.current.floor);
    },
    [resizeIfHolder],
  );

  return { state, setSink: useCallback((s: TerminalSink) => (sink.current = s), []), input, setTyping, take: () => floor("take"), fit };
}
