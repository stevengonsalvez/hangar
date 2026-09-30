// The Ctrl+Tab switcher's window side: while Ctrl is held it lists the
// focused pane's tabs (`recent_tabs.ts`) with one chosen, and letting go of
// Ctrl shows that one, as Orca's `RecentTabSwitcher` does
// (`orca:src/renderer/src/components/tab-bar/RecentTabSwitcher.tsx:56-147`).

import { createSignal, For, onCleanup, Show } from "solid-js";
import { releasesSwitcher, stepIndex } from "./recent_tabs.ts";

export interface SwitcherDeps {
  /** The focused pane's tabs in switcher order, and its shown tab; `null`
   * when there is nothing to switch between. */
  candidates(): { keys: string[]; shown: string | null } | null;
  /** A tab's title. */
  label(key: string): string;
  /** Show the tab chosen when Ctrl was let go. */
  commit(key: string): void;
}

/**
 * The switcher: `step` opens it on the first Ctrl+Tab and moves the choice
 * on each next one; Ctrl up commits; Esc, or the window losing focus, closes
 * it with nothing changed. Registers window listeners in the caller's owner.
 */
export function createSwitcher(deps: SwitcherDeps) {
  const [state, setState] = createSignal<{ keys: string[]; at: number } | null>(null);

  const step = (direction: 1 | -1) => {
    const current = state();
    if (current !== null) {
      setState({ ...current, at: stepIndex(current.keys.length, current.at, direction) });
      return;
    }
    const candidates = deps.candidates();
    // One tab has nowhere to switch to, as Orca opens no switcher for it
    // (`recent-tab-switching.ts:157-160`).
    if (candidates === null || candidates.keys.length <= 1) return;
    const from = candidates.shown === null ? -1 : candidates.keys.indexOf(candidates.shown);
    setState({ keys: candidates.keys, at: stepIndex(candidates.keys.length, from, direction) });
  };
  const cancel = () => setState(null);
  const commit = () => {
    const current = state();
    setState(null);
    const key = current?.keys[current.at];
    if (key !== undefined) deps.commit(key);
  };

  // Capture, ahead of the terminal: its Esc Esc must not see the Esc that
  // closes the switcher.
  const onKeyDown = (event: KeyboardEvent) => {
    if (state() === null || event.key !== "Escape") return;
    event.preventDefault();
    event.stopPropagation();
    cancel();
  };
  const onKeyUp = (event: KeyboardEvent) => {
    if (state() === null || !releasesSwitcher(event)) return;
    event.preventDefault();
    event.stopPropagation();
    commit();
  };
  window.addEventListener("keydown", onKeyDown, true);
  window.addEventListener("keyup", onKeyUp, true);
  window.addEventListener("blur", cancel);
  onCleanup(() => {
    window.removeEventListener("keydown", onKeyDown, true);
    window.removeEventListener("keyup", onKeyUp, true);
    window.removeEventListener("blur", cancel);
  });

  const view = () => (
    <Show when={state()}>
      {(shown) => (
        <div class="tab-switcher-backdrop">
          <div class="tab-switcher" role="listbox" aria-label="Switch tabs">
            <div class="tab-switcher-heading">Switch Tab</div>
            <For each={shown().keys}>
              {(key, index) => (
                <div
                  class="tab-switcher-option"
                  role="option"
                  data-key={key}
                  aria-selected={index() === shown().at}
                >
                  {deps.label(key)}
                </div>
              )}
            </For>
          </div>
        </div>
      )}
    </Show>
  );

  return { step, view };
}
