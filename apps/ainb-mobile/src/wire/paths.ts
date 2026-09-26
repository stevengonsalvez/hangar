// The two app-data directories the adapter hands the crate: where it keeps
// the pairing index, pairing tokens and the file-backed device key
// (custody), and where the connection log lives. Both sit under the app's
// private Documents directory (`expo-file-system`'s `Paths.document`):
// private to the app, kept across updates, removed with the app. On iOS the
// device key is in the keychain (the crate's custody backend) and the pairing
// index is a file in Documents; Documents is in the iCloud/iTunes backup
// unless the app marks it excluded, which the switch to the adapter must do
// for the custody directory. On Android `allowBackup: false` (app.json)
// covers it. The crate receives plain filesystem paths.
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
  const custody = new Directory(Paths.document, "ainb", "custody");
  const log = new Directory(Paths.document, "ainb", "log");
  for (const d of [custody, log]) if (!d.exists) d.create({ intermediates: true, idempotent: true });
  cached = { custodyDir: pathOf(custody.uri), logDir: pathOf(log.uri) };
  return cached;
}

/** Tests only. */
export function resetWirePaths() {
  cached = undefined;
}
