import { FakeWire } from "./fake";
import { wirePaths } from "./paths";
import type { WireClient } from "./types";

export type * from "./types";

/**
 * What the native adapter module (lane E's `src/wire/native.ts`) must export
 * for the switch to accept it. This is the app's own load-time shape check;
 * lane E's type guard over the binding itself runs inside `NativeWire`.
 */
interface NativeModule {
  NativeWire: new (opts: { custodyDir: string; logDir: string }) => WireClient;
}

export function isNativeModule(mod: unknown): mod is NativeModule {
  return typeof mod === "object" && mod !== null && typeof (mod as { NativeWire?: unknown }).NativeWire === "function";
}

/** Loaded lazily so a FakeWire build never touches the binding, and a missing adapter is loud, not silent. */
function loadNative(): NativeModule {
  let mod: unknown;
  try {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    mod = require("./native");
  } catch (e) {
    throw new Error(`the ainb-wire-mobile adapter is not present (${e instanceof Error ? e.message : String(e)}); run with EXPO_PUBLIC_FAKE_WIRE=1`);
  }
  if (!isNativeModule(mod)) throw new Error("src/wire/native does not export a NativeWire class; refusing to start with an unknown transport");
  return mod;
}

let instance: WireClient | undefined;

/** The one transport for the process: FakeWire behind `EXPO_PUBLIC_FAKE_WIRE=1`, else the native adapter over the linked crate. */
export function wire(): WireClient {
  if (instance) return instance;
  if (process.env.EXPO_PUBLIC_FAKE_WIRE === "1") {
    instance = new FakeWire();
    return instance;
  }
  const { NativeWire } = loadNative();
  instance = new NativeWire(wirePaths());
  return instance;
}

/** Tests swap in their own client so each one starts from a known world. */
export function setWire(client: WireClient | undefined) {
  instance = client;
}
