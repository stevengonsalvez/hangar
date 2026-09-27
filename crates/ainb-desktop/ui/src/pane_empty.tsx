import { Match, Switch } from "solid-js";
import type { EmptyPane as EmptyPaneView } from "./pane_empty.ts";

/**
 * The work area with no terminal tab open (`emptyPaneView` decides which):
 * the selected session and a button that opens its terminal the same way its
 * sidebar row does, or the prompt to choose one.
 */
export function EmptyPane(props: { view: EmptyPaneView; onOpen(sessionId: string): void }) {
  return (
    <Switch>
      <Match when={props.view.kind === "selected" && props.view}>
        {(selected) => (
          <div class="empty empty-selected" data-session={selected().sessionId}>
            <p class="empty-name">{selected().name}</p>
            <p class="empty-branch">{selected().branch}</p>
            <button type="button" class="empty-open" onClick={() => props.onOpen(selected().sessionId)}>
              Open terminal
            </button>
          </div>
        )}
      </Match>
      <Match when={props.view.kind === "choose"}>
        <p class="empty">Choose a session to open its terminal</p>
      </Match>
    </Switch>
  );
}
