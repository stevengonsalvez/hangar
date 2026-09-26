// Where the crate keeps what JS must never hold: the device key and pairing
// index (custody) and the connection log. Both live under the app's document
// directory (excluded from backup on Android by app.json, and never shared),
// created on first use. The crate receives plain filesystem paths.
import { Directory, Paths } from "expo-file-system";

export interface WirePaths {
  custodyDir: string;
  logDir: string;
}

/** Strip the `file://` scheme the file-system API reports; the crate wants a path. */
export function pathOf(uri: string): string {
  return decodeURIComponent(uri.replace(/^file:\/\//, "")).replace(/\/+$/, "");
}

let cached: WirePaths | undefined;

export function wirePaths(): WirePaths {
  if (cached) return cached;
  const custody = new Directory(Paths.document, "wire", "custody");
  const log = new Directory(Paths.document, "wire", "log");
  for (const d of [custody, log]) if (!d.exists) d.create({ intermediates: true, idempotent: true });
  cached = { custodyDir: pathOf(custody.uri), logDir: pathOf(log.uri) };
  return cached;
}

/** Tests only. */
export function resetWirePaths() {
  cached = undefined;
}
