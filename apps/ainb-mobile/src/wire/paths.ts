// The two app-data directories the native adapter hands the crate: where
// it keeps secrets (the device key, pairing tokens, the pairing index) and
// where the connection log lives. Both are under the app's private document
// directory (`expo-file-system`), so they survive updates, are excluded
// from nothing the platform backs up privately, and are removed with the
// app. On iOS the device key itself lives in the keychain (the crate's
// custody backend); the directory holds the index and file-backed secrets.
//
// `expo-file-system` is a dependency of the switch to the adapter: add
// `"expo-file-system": "~57.0.7"` (`npx expo install expo-file-system`,
// the SDK 57 line) to package.json when the app moves off `FakeWire`.

import { Paths } from "expo-file-system";

/** The directory every ainb secret and record lives under. */
export const APP_DATA_DIR = `${Paths.document.uri.replace(/\/$/, "")}/ainb`;

/** Where the crate keeps the pairing index, tokens, and the file-backed device key. */
export const CUSTODY_DIR = `${APP_DATA_DIR}/custody`;

/** Where the crate writes the connection log the diagnostics screen reads. */
export const LOG_DIR = `${APP_DATA_DIR}/log`;

/**
 * The adapter's paths as `file://` URIs stripped to plain paths, which is
 * what the crate's `custody_dir` and `log_dir` take. The crate creates the
 * directories itself with private permissions.
 */
export function wirePaths(): { custodyDir: string; logDir: string } {
  return { custodyDir: plainPath(CUSTODY_DIR), logDir: plainPath(LOG_DIR) };
}

export function plainPath(uri: string): string {
  return uri.startsWith("file://") ? decodeURIComponent(uri.slice("file://".length)) : uri;
}
