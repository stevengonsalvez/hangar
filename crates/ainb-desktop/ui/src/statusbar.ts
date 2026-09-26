// The status bar's own small projections. The health a state means is
// `sidecar.ts`'s (`banner`, `retryable`); this only names which of the four
// dots the bar paints for it, so the bar's CSS never pattern-matches the
// wire's state strings itself.

import type { SidecarState } from "./sidecar.ts";

/** Which dot the status bar paints: `ok` (mint), `connecting` (muted),
 * `warn` (amber, reconnecting on its own) or `error` (coral, needs a person). */
export type DaemonDotState = "ok" | "connecting" | "warn" | "error";

/** The dot for `state`, from `sidecar.ts`'s own `SidecarState`. */
export function daemonDotState(state: SidecarState): DaemonDotState {
  switch (state.state) {
    case "connected":
      return "ok";
    case "starting":
      return "connecting";
    case "reconnecting":
      return "warn";
    case "degraded":
    case "incompatible":
      return "error";
  }
}
