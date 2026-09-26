// One session's terminal over the wire (M1-13): attach, feed the sink from
// frames, arbitrate the floor, detach on unmount and before background.
import { useCallback, useEffect, useRef, useState } from "react";

import { beforeBackground } from "../lifecycle";
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
  /** Set when the daemon refused our input: who holds the floor. */
  denied?: FloorHolder;
  closed?: string;
  cols?: number;
  rows?: number;
}

export function useTerminal(hostId: HostId | undefined, sessionKey: SessionKey | undefined) {
  const wire = useWire();
  const sink = useRef<TerminalSink | undefined>(undefined);
  const stream = useRef<number | undefined>(undefined);
  const [state, setState] = useState<TerminalState>({ canType: false, typing: false, floor: { floorGen: 0 }, nativeClients: 0 });
  const patch = (p: Partial<TerminalState>) => setState((s) => ({ ...s, ...p }));

  const detach = useCallback(async () => {
    const id = stream.current;
    stream.current = undefined;
    if (id !== undefined && hostId) await wire.terminalDetach(hostId, id).catch(() => undefined);
    setState((s) => ({ ...s, streamId: undefined, typing: false }));
  }, [wire, hostId]);

  const attach = useCallback(async () => {
    if (!hostId || !sessionKey) return;
    const info: HostInfo = await wire.hostInfo(hostId).catch(() => ({ capabilities: [] }));
    const canType =
      info.scope?.base === "mobile+type" && info.capabilities.includes("terminal.stream") && info.capabilities.includes("terminal.input");
    const at = await wire.terminalAttach({ hostId, sessionKey });
    stream.current = at.streamId;
    patch({ streamId: at.streamId, canType, floor: at.floor, nativeClients: at.nativeClients, cols: at.cols, rows: at.rows, closed: undefined });
  }, [wire, hostId, sessionKey]);

  useEffect(() => {
    void attach().catch((e: unknown) => patch({ closed: String(e instanceof Error ? e.message : e) }));
    const off = beforeBackground(detach);
    return () => {
      off();
      void detach();
    };
  }, [attach, detach]);

  const onFrame = useCallback((frame: TerminalFrame) => {
    const s = sink.current;
    switch (frame.kind) {
      case "snapshot_start":
        s?.clear();
        patch({ cols: frame.cols, rows: frame.rows });
        break;
      case "snapshot_chunk":
      case "output":
        s?.write(frame.data);
        break;
      case "resize":
        patch({ cols: frame.cols, rows: frame.rows });
        break;
      case "data_gap":
        s?.clear(); // the daemon follows every gap with a fresh snapshot
        break;
      case "floor":
        patch({ floor: { holder: frame.holder, floorGen: frame.floorGen }, denied: undefined });
        break;
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
  }, []);

  useWireEvents(
    useCallback(
      (ev) => {
        if (ev.kind === "terminal_frame" && ev.hostId === hostId && ev.streamId === stream.current) onFrame(ev.frame);
      },
      [hostId, onFrame],
    ),
  );

  const input = useCallback(
    async (data: string) => {
      const id = stream.current;
      if (id === undefined || !hostId || !state.typing) return;
      const mine = state.floor.holder?.streamId === id ? state.floor.floorGen : undefined;
      const r = await wire.terminalInput({ hostId, streamId: id, floorGen: mine, data });
      if ("kind" in r) patch({ denied: r.holder, floor: { holder: r.holder, floorGen: r.floorGen } });
      else patch({ denied: undefined });
    },
    [wire, hostId, state.typing, state.floor],
  );

  const floor = useCallback(
    async (action: "acquire" | "release" | "take") => {
      const id = stream.current;
      if (id === undefined || !hostId) return;
      const r = await wire.terminalFloor({ hostId, streamId: id, action });
      if ("kind" in r) patch({ denied: r.holder, floor: { holder: r.holder, floorGen: r.floorGen } });
      else patch({ floor: r, denied: undefined, typing: action !== "release" });
    },
    [wire, hostId],
  );

  const setTyping = useCallback(
    async (on: boolean) => {
      patch({ typing: on });
      await floor(on ? "acquire" : "release");
    },
    [floor],
  );

  const fit = useCallback(
    async (cols: number, rows: number) => {
      const id = stream.current;
      if (id === undefined || !hostId) return;
      if (state.floor.holder?.streamId !== id) return; // only the floor holder resizes
      await wire.terminalResize({ hostId, streamId: id, cols, rows }).catch(() => undefined);
    },
    [wire, hostId, state.floor],
  );

  return { state, setSink: (s: TerminalSink) => (sink.current = s), input, setTyping, take: () => floor("take"), fit };
}
