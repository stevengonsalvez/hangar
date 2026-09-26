import { createContext, useContext, useEffect, useState, type ReactNode } from "react";

import { wire } from "./index";
import type { WireClient, WireEvent } from "./types";

const WireContext = createContext<WireClient | undefined>(undefined);

export function WireProvider({ client, children }: { client?: WireClient; children: ReactNode }) {
  return <WireContext.Provider value={client ?? wire()}>{children}</WireContext.Provider>;
}

export function useWire(): WireClient {
  const client = useContext(WireContext);
  if (!client) throw new Error("useWire outside WireProvider");
  return client;
}

/** Subscribe to wire events for the life of the component. */
export function useWireEvents(cb: (ev: WireEvent) => void) {
  const client = useWire();
  useEffect(() => client.onEvent(cb), [client, cb]);
}

/** Load once, reload on any wire event of the named kinds. */
export function useWireQuery<T>(load: (client: WireClient) => Promise<T>, reloadOn: WireEvent["kind"][] = []) {
  const client = useWire();
  const [state, setState] = useState<{ data?: T; error?: string }>({});
  useEffect(() => {
    let live = true;
    const run = () =>
      load(client).then(
        (data) => live && setState({ data }),
        (e: unknown) => live && setState({ error: String(e) }),
      );
    void run();
    const off = client.onEvent((ev) => {
      if (reloadOn.includes(ev.kind)) void run();
    });
    return () => {
      live = false;
      off();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client]);
  return state;
}
