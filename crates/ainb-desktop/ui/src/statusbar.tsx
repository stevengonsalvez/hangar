import { Show } from "solid-js";
import type { HostId } from "../../../ainb-app/bindings/AppState";
import type { SidecarState } from "./sidecar.ts";
import { daemonDotState } from "./statusbar.ts";

interface Props {
  host: HostId | undefined;
  sidecar: SidecarState;
  /** The board's Needs column (`countIn`): every agent blocked on a human. */
  needsYou: number;
  /** The board's Idle column (`countIn`). */
  idle: number;
  /** Frames the store refused (#1132): a development build only. */
  framesIgnored?: number;
}

/**
 * The status bar: 24px (`--h-statusbar`), the window's own footer rather
 * than a page's. What daemon this window is on, whether it is reachable, and
 * how many rows want a human, as one amber count rather than the four
 * separate ASK/APPROVE/WAIT/ERR badges the old header drew.
 */
export function Statusbar(props: Props) {
  return (
    <footer class="statusbar">
      <span class="statusbar-host" title="Host">
        {props.host ?? "no host"}
      </span>
      {/* A live region needs text: the dot alone announces nothing. */}
      <span
        class={`statusbar-daemon-dot ${daemonDotState(props.sidecar)}`}
        role="status"
        title={props.sidecar.state}
      >
        <span class="visually-hidden">Daemon {props.sidecar.state}</span>
      </span>
      <span class="statusbar-counts" aria-label="Attention">
        <Show when={props.needsYou > 0}>
          <span class="statusbar-needs">{props.needsYou} need you</span>
        </Show>
        <span class="statusbar-idle">{props.idle} idle</span>
        <Show when={props.framesIgnored !== undefined && props.framesIgnored > 0}>
          <span class="statusbar-ignored" title="Frames the store ignored">
            {props.framesIgnored} ignored
          </span>
        </Show>
      </span>
    </footer>
  );
}
