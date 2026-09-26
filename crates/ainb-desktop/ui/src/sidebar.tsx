import { createMemo, createSignal, For, Show } from "solid-js";
import type { SessionsView_Serialize } from "../../../ainb-app/bindings/AppState";
import { keyedList, sameKeys } from "./keyed.ts";
import { isSelected, label, ringFor, rowStatus } from "./sessions.ts";
import {
  agentLabel,
  formatGitChanges,
  projectGroups,
  readCollapsed,
  writeCollapsed,
  type ProjectGroup,
  type SidebarStorage,
  type WorktreeCard,
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
  // Drawn by key, never by object (#1267, `keyed.ts`): every frame rebuilds
  // the groups, cards and rows, and `For` keys by identity, so drawing the
  // objects would remount every row whenever any one changed, and a focused
  // row would drop the keyboard to <body>. Keys are equal frame to frame, so
  // the lists patch, and each row reads its current data through its key.
  const groups = createMemo(() => keyedList(projectGroups(props.sessions), (group) => group.path));
  const groupKeys = createMemo(() => groups().keys, [], { equals: sameKeys });
  const [collapsed, setCollapsed] = createSignal(readCollapsed(safeStorage()));
  /** `<details>` already flipped its own `open` before `onToggle` fires:
   * read the result back rather than track the click that caused it. */
  const onToggle = (path: string, open: boolean) => {
    const next = new Set(collapsed());
    if (open) next.delete(path);
    else next.add(path);
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
        when={groupKeys().length > 0}
        fallback={<p class="empty">{props.loading || !props.sessions ? "Loading sessions" : "No sessions"}</p>}
      >
        <For each={groupKeys()}>
          {(groupKey) => {
            const group = (): ProjectGroup | undefined => groups().byKey.get(groupKey);
            const cards = createMemo(() => keyedList(group()?.cards ?? [], (card) => card.key));
            const cardKeys = createMemo(() => cards().keys, [], { equals: sameKeys });
            return (
              <Show when={group()}>
                {(g) => (
                  <details
                    class="workspace"
                    data-workspace={g().name}
                    open={!collapsed().has(g().path)}
                    onToggle={(event) => onToggle(g().path, (event.currentTarget as HTMLDetailsElement).open)}
                  >
                    <summary class="workspace-name">
                      <span class="workspace-name-text">{label(g().name)}</span>
                      <span class="workspace-count">{g().sessionCount}</span>
                    </summary>
                    <ul class="worktree-cards">
                      {/* One card per worktree path; several sessions in it are
                          the card's inline agent rows, never separate cards. */}
                      <For each={cardKeys()}>
                        {(cardKey) => (
                          <Show when={cards().byKey.get(cardKey)}>
                            {(card) => <Card card={card()} sessions={props.sessions} onOpen={props.onOpen} />}
                          </Show>
                        )}
                      </For>
                    </ul>
                  </details>
                )}
              </Show>
            );
          }}
        </For>
      </Show>
    </aside>
  );
}

/** One worktree card: its meta line and one agent row per session in it,
 * rows keyed by session id for the same reason the groups are. */
function Card(props: {
  card: WorktreeCard;
  sessions: SessionsView_Serialize | undefined;
  onOpen(sessionId: string): void;
}) {
  const rows = createMemo(() => keyedList(props.card.sessions, (session) => session.id));
  const rowKeys = createMemo(() => rows().keys, [], { equals: sameKeys });
  return (
    <li class="worktree-card">
      <div class="worktree-card-title">{label(props.card.title)}</div>
      <div class="worktree-card-meta">
        <span class="branch">{label(props.card.branch)}</span>
        <Show when={props.card.gitChanges}>
          {(changes) => <span class="git-counts">{formatGitChanges(changes())}</span>}
        </Show>
        <Show when={props.card.model}>{(model) => <span class="model">{label(model())}</span>}</Show>
      </div>
      <ul class="agent-rows">
        <For each={rowKeys()}>
          {(rowKey) => (
            <Show when={rows().byKey.get(rowKey)}>
              {(session) => {
                const selected = () => isSelected(props.sessions, session().id);
                const ring = () => ringFor(session());
                return (
                  <li>
                    <button
                      type="button"
                      class="session-row"
                      classList={{ selected: selected() }}
                      data-session={session().id}
                      data-ring={ring()?.toLowerCase() ?? "none"}
                      aria-current={selected() ? "true" : undefined}
                      onClick={() => props.onOpen(session().id)}
                    >
                      <span
                        class={`ring ${rowStatus(session().status)}`}
                        title={ring() ?? rowStatus(session().status)}
                      />
                      <span class="agent-type">{agentLabel(session().agent_type)}</span>
                      <span class="name">{label(session().name)}</span>
                    </button>
                  </li>
                );
              }}
            </Show>
          )}
        </For>
      </ul>
    </li>
  );
}
