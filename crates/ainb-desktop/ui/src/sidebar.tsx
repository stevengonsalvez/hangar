import { createEffect, createMemo, createSignal, For, on, Show } from "solid-js";
import type {
  AgentCardFrame,
  FleetView_Serialize,
  SessionLabelStore,
  Session_Serialize,
  SessionsView_Serialize,
} from "../../../ainb-app/bindings/AppState";
import type { AckMap } from "./acks.ts";
import type { PendingWorktree } from "./composer.ts";
import { keyedList, sameKeys } from "./keyed.ts";
import { PrBadgeButton } from "./pr_badge.tsx";
import { opensRowMenu, rowMenuItems, sessionIn, type RowPick } from "./row_menu.ts";
import { RowMenu } from "./row_menu.tsx";
import { RenameField } from "./rename_field.tsx";
import { isSelected, label } from "./sessions.ts";
import { SidebarFilter } from "./sidebar_filter.tsx";
import { filterProjectGroups, hidesProject, hidesSession } from "./sidebar_filter.ts";
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
import { statusForSession } from "./status.ts";
import { StatusGlyph } from "./status_glyph.tsx";

interface Props {
  sessions: SessionsView_Serialize | undefined;
  stale: boolean;
  /** The host is loading workspaces (the WorkspaceLoad section). */
  loading: boolean;
  /** A worktree the composer is creating into a project, or `null`: drawn as
   * a working card at the top of that project's group until the request
   * settles. `main.tsx` holds it, not this component, so it survives the
   * composer's own view closing (Cancel keeps the create running). */
  pending: PendingWorktree | null;
  /** The agent status frame's own cards, for each row's `UiStatus`. Optional
   * so a caller with no status frame yet (or a test fixture) still draws the
   * row through `statusForSession`'s own fallback. */
  cards?: readonly AgentCardFrame[];
  /** The Fleet frame's per-session metadata, for the same join `board.ts`
   * uses (`provider_session_id` against a card's `session_key`). */
  fleetMetadata?: FleetView_Serialize["fleet_metadata"];
  /** This viewer's Done acks (`acks.ts`), the same map the board reads. */
  acks?: AckMap;
  /** A row was chosen: open its session's terminal tab. */
  onOpen(sessionId: string): void;
  /** Mod+N, or the button: open the new-worktree composer. */
  onNew(): void;
  /** An item was chosen in a row's context menu (`row_menu.ts`). Without
   * it a right-click on a row is left to the webview. */
  onRowPick?(pick: RowPick): void;
  /** The host's label store (the `session_labels` section): each card's
   * title, by its primary session's tmux name (`sidebar_model.ts`). */
  labels?: SessionLabelStore;
  /** Ask the host to rename `sessionId` to `name`, as typed. Resolves `null`
   * when it did, else the host's reason. Without it no title is editable. */
  onRename?(sessionId: string, name: string): Promise<string | null>;
  /** The host's `pr_badge` for a card's session. Without it, or without
   * `onOpenUrl`, no card draws a PR badge. */
  prBadge?(sessionId: string): Promise<unknown>;
  /** Open a PR badge's URL: the host's `open_url`, never the webview. */
  onOpenUrl?(url: string): void;
  /** The sidebar element, for Esc Esc to return focus to. */
  ref(element: HTMLElement): void;
}

/** Where a row's context menu is open, on which session, and the row to give
 * the keyboard back to when it closes. */
interface MenuAt {
  session: Session_Serialize;
  /** The worktree's name as its card shows it: what Copy Worktree Name copies. */
  name: string;
  /** The card's key: the card whose title Rename edits. */
  card: string;
  x: number;
  y: number;
  row: HTMLElement;
  /** The sidebar, for the keyboard when the row itself is gone. */
  sidebar: HTMLElement | null;
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
  const allGroups = createMemo(() => projectGroups(props.sessions, props.labels));
  // The filter's text: a signal, never storage, so a relaunch opens the whole
  // list, as Orca's own search drops its query when its surface closes. It
  // only hides rows: the selection is the host's and stays where it was.
  const [query, setQuery] = createSignal("");
  const kept = createMemo(() => filterProjectGroups(allGroups(), query()));
  const groups = createMemo(() => keyedList(kept(), (group) => group.path));
  const cardCount = (from: readonly ProjectGroup[]) => from.reduce((sum, group) => sum + group.cards.length, 0);
  const matched = () => (kept() === allGroups() ? null : cardCount(kept()));

  // Orca lifts the sidebar filters hiding a workspace it activates
  // (`worktree-activation.ts`), since a target that is not drawn cannot be
  // revealed. The same here, on a CHANGE only: the host moving the selection
  // to a row the query hides, or a create landing in a project it hides.
  // Typing a query that hides the current selection is the person's own
  // choice and stays. The memos pass on a value only when it changes, so a
  // frame repeating the same selection does not count as a move.
  const selectedId = createMemo(() => props.sessions?.selected_session_id ?? null);
  createEffect(
    on(
      selectedId,
      (id) => {
        if (id !== null && hidesSession(allGroups(), query(), id)) setQuery("");
      },
      { defer: true },
    ),
  );
  const pendingPath = createMemo(() => props.pending?.projectPath ?? null);
  createEffect(
    on(pendingPath, (path) => {
      if (path !== null && hidesProject(allGroups(), query(), path)) setQuery("");
    }),
  );
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

  /** The pending card belongs to `group` when the composer's project matches
   * it; the object itself (not just a boolean) so the card can label itself. */
  const pendingIn = (group: ProjectGroup): PendingWorktree | null =>
    props.pending && props.pending.projectPath === group.path ? props.pending : null;

  /** Why the list is empty: the host has not answered yet, it has nothing, or
   * it has rows and the filter hides every one of them. */
  const emptyText = () => {
    if (allGroups().length > 0) return "No worktrees match";
    return props.loading || !props.sessions ? "Loading sessions" : "No sessions";
  };

  // One row menu at a time, Orca's `CLOSE_ALL_CONTEXT_MENUS_EVENT` without the
  // event: a second right-click replaces the first.
  const [menu, setMenu] = createSignal<MenuAt | null>(null);
  const openMenu = (at: MenuAt) => setMenu(at);
  /** Close the menu. With `restore`, give the keyboard back to the row (the
   * sidebar when the row is gone), as Orca's `handleCloseAutoFocus` keeps it
   * on the sidebar rather than the body. */
  const closeMenu = (restore: boolean) => {
    const at = menu();
    setMenu(null);
    if (at && restore) (at.row.isConnected ? at.row : at.sidebar)?.focus();
  };
  // A menu outlives no drain that drops its row: Open in Editor selects the
  // row first and the editor opens whatever is selected, so a pick on a row
  // that is gone could open another session's worktree. The row is gone with
  // it, so the keyboard goes back to the sidebar itself.
  createEffect(() => {
    const at = menu();
    if (at && !sessionIn(props.sessions, at.session.id)) {
      setMenu(null);
      at.sidebar?.focus();
    }
  });

  // The card whose title is a rename field, by its worktree key (the card's
  // own `key`, not the keyed list's); one at a time, as Orca has one editor
  // open. A card that leaves what is drawn (gone, or hidden by the filter)
  // takes its field with it, rather than reopening it later with the keyboard.
  const [renaming, setRenaming] = createSignal<string | null>(null);
  createEffect(() => {
    const key = renaming();
    if (key !== null && !kept().some((group) => group.cards.some((card) => card.key === key))) setRenaming(null);
  });

  return (
    <aside class="sidebar" aria-label="Sessions" tabIndex={-1} ref={props.ref}>
      <div class="sidebar-head">
        <span class="sidebar-title">Projects</span>
        <button type="button" class="sidebar-new" onClick={props.onNew}>
          + New
        </button>
        <Show when={props.stale}>
          <span class="stale">stale</span>
        </Show>
      </div>
      <SidebarFilter query={query()} onQuery={setQuery} matched={matched()} total={cardCount(allGroups())} />
      <Show when={groupKeys().length > 0} fallback={<p class="empty">{emptyText()}</p>}>
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
                      {/* The composer's own card while it creates into this
                          project: ahead of every real card, so the person who
                          just asked for it sees it land where they look. */}
                      <Show when={pendingIn(g())}>
                        {(pending) => (
                          <li class="worktree-card" data-pending="true">
                            <div class="worktree-card-title">
                              <span class="spinner" aria-hidden="true" />
                              {label(pending().name)}
                            </div>
                          </li>
                        )}
                      </Show>
                      {/* One card per worktree path; several sessions in it are
                          the card's inline agent rows, never separate cards. */}
                      <For each={cardKeys()}>
                        {(cardKey) => (
                          <Show when={cards().byKey.get(cardKey)}>
                            {(card) => (
                              <Card
                                card={card()}
                                sessions={props.sessions}
                                cards={props.cards}
                                fleetMetadata={props.fleetMetadata}
                                acks={props.acks}
                                onOpen={props.onOpen}
                                onMenu={props.onRowPick ? openMenu : undefined}
                                renaming={renaming() === card().key}
                                onRename={props.onRename}
                                onRenameStart={() => setRenaming(card().key)}
                                // Only its own field: a commit that lands after
                                // another card's field opened leaves that one.
                                onRenameDone={() => setRenaming((key) => (key === card().key ? null : key))}
                                prBadge={props.prBadge}
                                onOpenUrl={props.onOpenUrl}
                              />
                            )}
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
      {/* Keyed by the opening, so each one mounts fresh and focuses its first
          item, and a second right-click moves the menu rather than adding one. */}
      <For each={menu() ? [menu()!] : []}>
        {(at) => (
          <RowMenu
            x={at.x}
            y={at.y}
            label={`${label(at.name)} actions`}
            items={rowMenuItems(at.session)}
            onClose={closeMenu}
            onPick={(action) => {
              closeMenu(true);
              if (action === "rename") return setRenaming(at.card);
              // The session as the latest frame has it, not as it was when
              // the menu opened.
              const session = sessionIn(props.sessions, at.session.id) ?? at.session;
              props.onRowPick?.({ action, session, name: at.name });
            }}
          />
        )}
      </For>
    </aside>
  );
}

/** One worktree card: its meta line and one agent row per session in it,
 * rows keyed by session id for the same reason the groups are. */
function Card(props: {
  card: WorktreeCard;
  sessions: SessionsView_Serialize | undefined;
  cards?: readonly AgentCardFrame[];
  fleetMetadata?: FleetView_Serialize["fleet_metadata"];
  acks?: AckMap;
  onOpen(sessionId: string): void;
  onMenu?(at: MenuAt): void;
  /** This card's title is the rename field. */
  renaming: boolean;
  onRename?(sessionId: string, name: string): Promise<string | null>;
  onRenameStart(): void;
  onRenameDone(): void;
  prBadge?(sessionId: string): Promise<unknown>;
  onOpenUrl?(url: string): void;
}) {
  let item!: HTMLLIElement;
  const rows = createMemo(() => keyedList(props.card.sessions, (session) => session.id));
  const rowKeys = createMemo(() => rows().keys, [], { equals: sameKeys });
  const menuFor = (row: HTMLElement, x: number, y: number) => {
    const session = props.card.sessions.find((candidate) => candidate.id === row.dataset.session);
    if (!props.onMenu || !session) return false;
    props.onMenu({
      session,
      name: props.card.title,
      card: props.card.key,
      x,
      y,
      row,
      sidebar: row.closest<HTMLElement>(".sidebar"),
    });
    return true;
  };
  /** A right-click anywhere on the card, as on Orca's: on a row it acts on
   * that row's session, elsewhere on the card's first. */
  const onContextMenu = (event: MouseEvent) => {
    // The PR badge is the branch's, not a row's: no row menu opens on it.
    if ((event.target as Element).closest(".pr-badge")) return;
    const card = event.currentTarget as HTMLElement;
    const row =
      (event.target as Element).closest<HTMLElement>(".session-row") ?? card.querySelector<HTMLElement>(".session-row");
    if (row && menuFor(row, event.clientX, event.clientY)) event.preventDefault();
  };
  /** The keyboard's way in: the menu opens under the focused row. */
  const onRowKeyDown = (event: KeyboardEvent) => {
    if (!opensRowMenu(event)) return;
    const row = event.currentTarget as HTMLElement;
    const rect = row.getBoundingClientRect();
    if (menuFor(row, rect.left, rect.bottom)) event.preventDefault();
  };
  /** The field is done. From Enter or Esc the keyboard goes back to the row
   * the title names, so it stays in the sidebar rather than on the body. */
  const renameDone = (restore: boolean) => {
    const id = props.card.primaryId;
    props.onRenameDone();
    if (restore) [...item.querySelectorAll<HTMLElement>(".session-row")].find((row) => row.dataset.session === id)?.focus();
  };
  return (
    <li class="worktree-card" ref={item} onContextMenu={onContextMenu}>
      <Show
        when={props.renaming && props.onRename}
        fallback={
          <div class="worktree-card-title" onDblClick={() => props.onRename && props.onRenameStart()}>
            {label(props.card.title)}
          </div>
        }
      >
        {(rename) => {
          // Taken once: the field can still be answering after this branch
          // is gone, and a read of `rename` then is a stale read.
          const submit = rename();
          return (
            <RenameField
              current={props.card.title}
              onSubmit={(name) => submit(props.card.primaryId, name)}
              onDone={renameDone}
            />
          );
        }}
      </Show>
      <div class="worktree-card-meta">
        <span class="branch">{label(props.card.branch)}</span>
        <Show when={props.card.gitChanges}>
          {(changes) => <span class="git-counts">{formatGitChanges(changes())}</span>}
        </Show>
        <Show when={props.card.model}>{(model) => <span class="model">{label(model())}</span>}</Show>
        {/* Beside the branch it is for, outside every row's button: a click
            on it opens the PR and never a row. */}
        <Show when={props.prBadge && props.onOpenUrl ? { fetch: props.prBadge, open: props.onOpenUrl } : null}>
          {(host) => (
            <PrBadgeButton
              sessionId={props.card.primaryId}
              branch={props.card.branch}
              fetch={host().fetch}
              onOpenUrl={host().open}
            />
          )}
        </Show>
      </div>
      <ul class="agent-rows">
        <For each={rowKeys()}>
          {(rowKey) => (
            <Show when={rows().byKey.get(rowKey)}>
              {(session) => {
                const selected = () => isSelected(props.sessions, session().id);
                // The same vocabulary the board's card and the tab chip read,
                // joined the same way: a row with no matching card still falls
                // back to its own ring and lifecycle, never to nothing.
                const status = () =>
                  statusForSession(session(), props.cards ?? [], props.fleetMetadata, props.acks ?? {});
                return (
                  <li>
                    <button
                      type="button"
                      class="session-row"
                      classList={{ selected: selected() }}
                      data-session={session().id}
                      aria-current={selected() ? "true" : undefined}
                      onClick={() => props.onOpen(session().id)}
                      onKeyDown={onRowKeyDown}
                      aria-haspopup="menu"
                    >
                      <StatusGlyph status={status()} />
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
