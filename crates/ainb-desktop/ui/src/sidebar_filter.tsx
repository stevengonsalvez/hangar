import { createEffect, createSignal, onCleanup, Show } from "solid-js";

/** How long the match count waits for typing to pause before it is spoken:
 * a polite region that changes on every keystroke talks without end. */
export const ANNOUNCE_DEBOUNCE_MS = 400;

interface Props {
  /** The text typed so far; the sidebar holds it, so it resets with the window. */
  query: string;
  onQuery(query: string): void;
  /** Worktrees the query keeps, or `null` while it filters nothing. */
  matched: number | null;
  /** Worktrees in the unfiltered list. */
  total: number;
}

function announcement(matched: number, total: number): string {
  return matched === 0 ? "No worktrees match" : `${matched} of ${total} worktrees match`;
}

/**
 * The sidebar's filter field, Orca's workspace search field
 * (`WorkspaceKanbanSearchField.tsx`) in the sidebar: typing narrows the list
 * as it goes, Escape clears a filled field, and a clear button empties it
 * without taking the keyboard out of it, and while it filters it shows
 * `n / m` and says the same, after a pause, to a screen reader. What a query
 * matches is `sidebar_filter.ts`'s.
 */
export function SidebarFilter(props: Props) {
  let input: HTMLInputElement | undefined;
  const clear = () => props.onQuery("");
  const [spoken, setSpoken] = createSignal("");
  createEffect(() => {
    const matched = props.matched;
    const total = props.total;
    if (matched === null) {
      setSpoken("");
      return;
    }
    const timer = setTimeout(() => setSpoken(announcement(matched, total)), ANNOUNCE_DEBOUNCE_MS);
    onCleanup(() => clearTimeout(timer));
  });

  const onKeyDown = (event: KeyboardEvent) => {
    // Mid-composition Escape belongs to the IME, which cancels the reading
    // being composed rather than the query behind it.
    if (event.key !== "Escape" || event.isComposing) return;
    // An empty field has nothing of its own to cancel: the Escape goes on to
    // whoever else listens for one.
    if (props.query === "") return;
    event.preventDefault();
    clear();
  };

  return (
    <div class="sidebar-filter" role="search" classList={{ filtering: props.matched !== null }}>
      <input
        ref={input}
        type="text"
        class="sidebar-filter-input"
        aria-label="Filter worktrees"
        placeholder="Filter worktrees"
        spellcheck={false}
        autocomplete="off"
        value={props.query}
        onInput={(event) => props.onQuery(event.currentTarget.value)}
        onKeyDown={onKeyDown}
      />
      <Show when={props.matched !== null}>
        <span class="sidebar-filter-count" aria-hidden="true">
          {props.matched} / {props.total}
        </span>
      </Show>
      <Show when={props.query !== ""}>
        <button
          type="button"
          class="sidebar-filter-clear"
          aria-label="Clear filter"
          // Clearing removes this button, so focus would fall to <body>:
          // keep it in the field the person is still typing in.
          onMouseDown={(event) => event.preventDefault()}
          onClick={() => {
            clear();
            input?.focus();
          }}
        >
          ×
        </button>
      </Show>
      <div class="visually-hidden" role="status" aria-live="polite">
        {spoken()}
      </div>
    </div>
  );
}
