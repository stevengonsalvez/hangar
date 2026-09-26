import { createSignal, For, Show } from "solid-js";
import type { SessionsView_Serialize } from "../../../ainb-app/bindings/AppState";
import { isSelected, label, ringFor, rowStatus } from "./sessions.ts";
import {
  agentLabel,
  formatGitChanges,
  projectGroups,
  readCollapsed,
  writeCollapsed,
  type SidebarStorage,
} from "./sidebar_model.ts";

interface Props {
  sessions: SessionsView_Serialize | undefined;
  stale: boolean;
  /** The host is loading workspaces (the WorkspaceLoad section). */
  loading: boolean;
  /** A row was chosen: open its session's terminal tab. */
  onOpen(sessionId: string): void;
  /** The sidebar element, for Esc Esc to return focus to. */
  ref(element: HTMLElement): void;
}

/**
 * `window.localStorage`, or `undefined` when it is missing, throws (a
 * private window, blocked site data), or this module evaluates with no
 * `window` at all (the SSR parity test): the same guard `theme.ts` uses for
 * its own per-viewer choice.
 */
function safeStorage(): SidebarStorage | undefined {
  try {
    return window.localStorage;
  } catch {
    return undefined;
  }
}

/**
 * The sidebar: Orca's project groups over worktree cards, each card the
 * sessions sharing one worktree path folded together (`sidebar_model.ts`).
 * Selection and attention are the Sessions section's own; the sidebar draws
 * them and keeps none.
 *
 * A session is a `<button>`, so a click or Enter opens its terminal tab. A
 * project group is a native `<details>`: the disclosure triangle, its
 * keyboard handling and which click toggles it are the platform's, and this
 * component only remembers the result, per viewer, across a relaunch.
 */
export function Sidebar(props: Props) {
  const groups = () => projectGroups(props.sessions);
  const [collapsed, setCollapsed] = createSignal(readCollapsed(safeStorage()));
  /** `<details>` already flipped its own `open` before `onToggle` fires:
   * read the result back rather than track the click that caused it. */
  const onToggle = (name: string, open: boolean) => {
    const next = new Set(collapsed());
    if (open) next.delete(name);
    else next.add(name);
    setCollapsed(next);
    writeCollapsed(safeStorage(), next);
  };

  return (
    <aside class="sidebar" aria-label="Sessions" tabIndex={-1} ref={props.ref}>
      <div class="sidebar-head">
        <span class="sidebar-title">Projects</span>
        <Show when={props.stale}>
          <span class="stale">stale</span>
        </Show>
      </div>
      <Show
        when={groups().length > 0}
        fallback={<p class="empty">{props.loading || !props.sessions ? "Loading sessions" : "No sessions"}</p>}
      >
        <For each={groups()}>
          {(group) => (
            <details
              class="workspace"
              data-workspace={group.name}
              open={!collapsed().has(group.name)}
              onToggle={(event) => onToggle(group.name, (event.currentTarget as HTMLDetailsElement).open)}
            >
              <summary class="workspace-name">
                <span class="workspace-name-text">{label(group.name)}</span>
                <span class="workspace-count">{group.sessionCount}</span>
              </summary>
              <ul class="worktree-cards">
                {/* One card per worktree path; several sessions in it are the
                    card's inline agent rows, never separate cards (#P2). */}
                <For each={group.cards}>
                  {(card) => (
                    <li class="worktree-card">
                      <div class="worktree-card-title">{label(card.title)}</div>
                      <div class="worktree-card-meta">
                        <span class="branch">{label(card.branch)}</span>
                        <Show when={card.gitChanges}>
                          {(changes) => <span class="git-counts">{formatGitChanges(changes())}</span>}
                        </Show>
                        <Show when={card.model}>{(model) => <span class="model">{label(model())}</span>}</Show>
                      </div>
                      <ul class="agent-rows">
                        <For each={card.sessions}>
                          {(session) => {
                            const selected = () => isSelected(props.sessions, session.id);
                            const ring = () => ringFor(session);
                            return (
                              <li>
                                <button
                                  type="button"
                                  class="session-row"
                                  classList={{ selected: selected() }}
                                  data-session={session.id}
                                  data-ring={ring()?.toLowerCase() ?? "none"}
                                  aria-current={selected() ? "true" : undefined}
                                  onClick={() => props.onOpen(session.id)}
                                >
                                  <span
                                    class={`ring ${rowStatus(session.status)}`}
                                    title={ring() ?? rowStatus(session.status)}
                                  />
                                  <span class="agent-type">{agentLabel(session.agent_type)}</span>
                                  <span class="name">{label(session.name)}</span>
                                </button>
                              </li>
                            );
                          }}
                        </For>
                      </ul>
                    </li>
                  )}
                </For>
              </ul>
            </details>
          )}
        </For>
      </Show>
    </aside>
  );
}
