import { Show } from "solid-js";

interface Props {
  /** The text typed so far; the sidebar holds it, so it resets with the window. */
  query: string;
  onQuery(query: string): void;
}

/**
 * The sidebar's filter field, Orca's workspace search field
 * (`WorkspaceKanbanSearchField.tsx`) in the sidebar: typing narrows the list
 * as it goes, Escape clears a filled field, and a clear button empties it
 * without taking the keyboard out of it. What a query matches is
 * `sidebar_filter.ts`'s.
 */
export function SidebarFilter(props: Props) {
  let input: HTMLInputElement | undefined;
  const clear = () => props.onQuery("");

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
    <div class="sidebar-filter">
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
    </div>
  );
}
