import { createEffect, createSignal, For, on, onCleanup, Show } from "solid-js";
import { SPAWN_AGENTS } from "./composer.ts";
import type { NewAgentFlow } from "./new_agent.ts";
import { NO_SESSION } from "./shell_tab.ts";
import type { WorktreeTarget } from "./worktree_target.ts";

interface Props {
  /** The worktree both items act on (`worktreeTarget` of this pane's shown
   * tab). `null`: nothing to open in, so the menu is off and says why. */
  target: WorktreeTarget | null;
  mac: boolean;
  /** "New terminal": a plain shell in the worktree `target` names. */
  onNewTerminal(target: WorktreeTarget): void;
  /** "+ agent": one more agent in the target's worktree. */
  agents: NewAgentFlow;
  /** Where the keyboard goes after a pick: the shown tab, as the palette and
   * the composer hand it back. */
  restoreFocus(): void;
}

/**
 * A pane strip's one "+", as Orca's: a "New tab" menu with New terminal, a
 * separator, then the agents, default first
 * (`orca:src/renderer/src/components/tab-bar/tab-bar-surface.tsx:205-215,270-277`,
 * `QuickLaunchButton.tsx:46-58,203-230`). This window has no agent detection
 * or per-agent enable switch yet, so it lists the daemon's `SpawnAgent`s in
 * the composer's order, Claude (its default) first. No model and no prompt:
 * Orca asks for neither there. An agent is off while an add is on the host,
 * as Orca's item is while its launch is pending.
 *
 * One per pane strip (`Panes`' `stripEnd`), each on that pane's shown tab. `position: fixed` under the "+", since the strip scrolls and
 * would clip it; it closes on any scroll or resize rather than float away.
 */
export function TabCreateMenu(props: Props) {
  const [at, setAt] = createSignal<{ left: number; top: number } | null>(null);
  let trigger: HTMLButtonElement | undefined;
  let menu: HTMLDivElement | undefined;

  const close = () => setAt(null);
  const outside = (event: PointerEvent) => {
    const target = event.target as Node | null;
    if (target && !menu?.contains(target) && !trigger?.contains(target)) close();
  };
  document.addEventListener("pointerdown", outside);
  window.addEventListener("resize", close);
  window.addEventListener("scroll", close, true);
  onCleanup(() => {
    document.removeEventListener("pointerdown", outside);
    window.removeEventListener("resize", close);
    window.removeEventListener("scroll", close, true);
  });
  // A menu opened for one worktree never acts on another.
  createEffect(on(() => props.target, close, { defer: true }));

  const items = () => [...(menu?.querySelectorAll<HTMLButtonElement>("[role=menuitem]:not(:disabled)") ?? [])];
  const toggle = () => {
    if (at() !== null || trigger === undefined) return close();
    const box = trigger.getBoundingClientRect();
    setAt({ left: box.left, top: box.bottom + 4 });
    queueMicrotask(() => items()[0]?.focus());
  };
  const onKey = (event: KeyboardEvent) => {
    const all = items();
    const at = all.indexOf(document.activeElement as HTMLButtonElement);
    const go = (index: number) => {
      event.preventDefault();
      all[(index + all.length) % all.length]?.focus();
    };
    if (event.key === "Escape") {
      event.preventDefault();
      close();
      trigger?.focus();
    } else if (event.key === "Tab") close();
    else if (event.key === "ArrowDown") go(at + 1);
    else if (event.key === "ArrowUp") go(at - 1);
    else if (event.key === "Home") go(0);
    else if (event.key === "End") go(all.length - 1);
  };
  /** Close, hand the keyboard back, then act on the target the menu was for. */
  const pick = (run: (target: WorktreeTarget) => void) => {
    const target = props.target;
    close();
    props.restoreFocus();
    if (target !== null) run(target);
  };
  const chord = () => (props.mac ? "⌘T" : "Ctrl+Shift+T");

  return (
    <span class="tab tab-create">
      <button
        ref={trigger}
        type="button"
        class="tab-new"
        aria-label="New tab"
        aria-haspopup="menu"
        aria-expanded={at() !== null}
        disabled={props.target === null}
        title={props.target === null ? NO_SESSION : "New tab"}
        onClick={toggle}
      >
        +
      </button>
      <Show when={at()}>
        {(place) => (
          <div
            ref={menu}
            class="tab-create-menu"
            role="menu"
            aria-label="New tab"
            style={{ left: `${place().left}px`, top: `${place().top}px` }}
            onKeyDown={onKey}
          >
            <button type="button" role="menuitem" data-item="terminal" onClick={() => pick((target) => props.onNewTerminal(target))}>
              <span>New terminal</span>
              <kbd>{chord()}</kbd>
            </button>
            <div role="separator" class="tab-create-separator" />
            <For each={SPAWN_AGENTS}>
              {(agent) => (
                <button
                  type="button"
                  role="menuitem"
                  data-agent={agent.id}
                  disabled={props.agents.adding()}
                  title={`Launch ${agent.label} in this worktree`}
                  onClick={() => pick((target) => props.agents.add(target, agent.id))}
                >
                  {agent.label}
                </button>
              )}
            </For>
          </div>
        )}
      </Show>
    </span>
  );
}
