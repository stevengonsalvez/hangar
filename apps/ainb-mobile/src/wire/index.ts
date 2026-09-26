import { FakeWire } from "./fake";
import type { WireClient } from "./types";

export type * from "./types";

let instance: WireClient | undefined;

/** The one transport for the process: FakeWire until the native binding is linked. */
export function wire(): WireClient {
  if (instance) return instance;
  if (process.env.EXPO_PUBLIC_FAKE_WIRE !== "1") {
    // The ubrn binding for `ainb-wire-mobile` is not linked yet (M1-02); a
    // build without the fake switch is deliberately loud rather than silent.
    throw new Error("ainb-wire-mobile is not linked; run with EXPO_PUBLIC_FAKE_WIRE=1");
  }
  instance = new FakeWire();
  return instance;
}

/** Tests swap in their own FakeWire so each one starts from a known world. */
export function setWire(client: WireClient | undefined) {
  instance = client;
}
