// The bridge between the React Native side and the xterm engine inside the
// webview. App-internal, not the daemon wire: bytes arrive here already
// decoded by the crate and cross the bridge as base64 because the bridge is
// string-only.

export type ToEngine =
  | { t: "write"; b64: string }
  | { t: "clear" }
  | { t: "fit" }
  | { t: "readonly"; on: boolean };

export type FromEngine =
  | { t: "ready" }
  | { t: "fit"; cols: number; rows: number }
  | { t: "input"; data: string };

export function encode(msg: ToEngine | FromEngine): string {
  return JSON.stringify(msg);
}

export function decodeFromEngine(raw: string): FromEngine | undefined {
  try {
    const v = JSON.parse(raw) as Partial<FromEngine>;
    if (v.t === "ready") return { t: "ready" };
    if (v.t === "fit" && typeof v.cols === "number" && typeof v.rows === "number") return { t: "fit", cols: v.cols, rows: v.rows };
    if (v.t === "input" && typeof v.data === "string") return { t: "input", data: v.data };
  } catch {
    // not ours
  }
  return undefined;
}

export function decodeToEngine(raw: string): ToEngine | undefined {
  try {
    const v = JSON.parse(raw) as Partial<ToEngine>;
    if (v.t === "write" && typeof v.b64 === "string") return { t: "write", b64: v.b64 };
    if (v.t === "clear") return { t: "clear" };
    if (v.t === "fit") return { t: "fit" };
    if (v.t === "readonly" && typeof v.on === "boolean") return { t: "readonly", on: v.on };
  } catch {
    // not ours
  }
  return undefined;
}

export function fromBase64(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export function toBase64(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]!);
  return btoa(bin);
}

/** The JS the host injects to hand the engine one message. */
export function injection(msg: ToEngine): string {
  return `window.__ainb&&window.__ainb.receive(${JSON.stringify(encode(msg))});true;`;
}

/** Inverse of `injection`, for tests that watch the bridge. */
export function decodeInjection(js: string): ToEngine | undefined {
  const m = /receive\((".*")\);true;$/.exec(js);
  if (!m) return undefined;
  return decodeToEngine(JSON.parse(m[1]!) as string);
}
