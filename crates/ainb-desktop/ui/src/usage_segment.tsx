import { For, Match, Show, Switch } from "solid-js";
import type { UsageView } from "../../../ainb-app/bindings/AppState";
import { usageSegment } from "./usage_segment.ts";

interface Props {
  /** Section 21, the very body the stats tab draws, or undefined until framed. */
  usage: UsageView | undefined;
  /** The section was withheld for being over the frame ceiling. */
  stale: boolean;
  /** Show the stats tab: the window's own handler, as its Stats tab calls. */
  onOpen: () => void;
}

/**
 * The status bar's usage segment (Orca O15, `StatusBarProviderSegment.tsx`):
 * one button over a chip per provider that opens the stats tab, as Orca's
 * opens its usage popover.
 *
 * It holds no subscription and asks the host for nothing: `main.tsx` hands it
 * the accessor the stats tab reads, so both move on the same frame.
 */
export function UsageSegment(props: Props) {
  const view = () => usageSegment(props.usage, props.stale);
  return (
    <button
      type="button"
      class="statusbar-usage"
      data-state={view().state}
      title={view().title}
      onClick={() => props.onOpen()}
    >
      <span class="statusbar-usage-label">Usage</span>
      <Switch>
        <Match when={view().state === "loading"}>
          <span class="statusbar-usage-pending">···</span>
        </Match>
        <Match when={view().state === "unavailable"}>
          <span class="statusbar-usage-none">--</span>
        </Match>
        <Match when={view().state === "failed"}>
          <Warn />
          <span>Refresh failed</span>
        </Match>
        <Match when={view().state === "empty"}>
          <span class="statusbar-usage-none">none</span>
        </Match>
        <Match when={view().state === "ready"}>
          <For each={view().chips}>
            {(chip) => (
              <span class="statusbar-usage-chip">
                <span class="statusbar-usage-provider">{chip.provider}</span> {chip.figure}
              </span>
            )}
          </For>
          <Show when={view().more > 0}>
            <span class="statusbar-usage-more" title={view().moreTitle}>
              +{view().more}
            </span>
          </Show>
          <Show when={view().stale}>
            <span class="statusbar-usage-stale">
              <Warn />
              <span class="visually-hidden">stale</span>
            </span>
          </Show>
        </Match>
      </Switch>
    </button>
  );
}

function Warn() {
  return (
    <span class="statusbar-usage-warn" aria-hidden="true">
      ⚠
    </span>
  );
}
