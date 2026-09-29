import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import type { ISearchOptions, SearchAddon } from "@xterm/addon-search";

/** The keys a find chord needs: `KeyboardEvent` fits. */
interface KeyLike {
  code: string;
  metaKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
}

/**
 * Whether `event` opens a terminal's find bar: Cmd+F on macOS, as Orca binds
 * it (`terminal.search`, `Mod+F`). Off macOS it is Ctrl+Shift+F, like every
 * shell chord in `tabs.ts`: plain Ctrl+F belongs to the pane (forward-char in
 * a shell, page-down in vim and less).
 */
export function findChord(event: KeyLike, mac: boolean): boolean {
  if (event.code !== "KeyF" || event.altKey) return false;
  return mac ? event.metaKey && !event.ctrlKey && !event.shiftKey : event.ctrlKey && event.shiftKey && !event.metaKey;
}

/** Orca's cap on a find query (`find-query-bounds.ts`): past it, nothing is searched. */
const FIND_QUERY_MAX_BYTES = 2 * 1024;

type Results = { resultIndex: number; resultCount: number };

// xterm reports index -1 when the matches pass its highlight limit.
const NO_RESULTS: Results = { resultIndex: -1, resultCount: 0 };

/** What the bar says about `results` for `query`, in Orca's words. */
export function matchStatus(query: string | null, results: Results): string {
  if (!query) return "0/0";
  if (results.resultCount === 0) return "No results";
  if (results.resultIndex === -1) return `${results.resultCount}+`;
  return `${results.resultIndex + 1}/${results.resultCount}`;
}

/**
 * Runs one find, dropping only xterm's decoration error: a match that starts
 * past a narrowed viewport makes a negative-width highlight, and xterm throws
 * rather than drawing it. The match is still found; only its paint is lost.
 */
function safeFind(find: () => boolean): boolean {
  try {
    return find();
  } catch (error) {
    if (error instanceof Error && /only accepts positive integers/i.test(error.message)) return false;
    throw error;
  }
}

/** Clears the highlights and the selection xterm leaves on the last match. */
function clearFind(addon: SearchAddon): void {
  addon.clearDecorations();
  addon.findNext("");
}

interface Props {
  addon: SearchAddon;
  open: boolean;
  mac: boolean;
  /** Esc or the close button: the pane closes the bar and takes the keyboard back. */
  onClose(): void;
  /** Hands the pane the query field, so Cmd+F on an open bar can reselect it. */
  inputRef(input: HTMLInputElement): void;
}

/**
 * A terminal's find bar, ported from Orca's `TerminalSearch.tsx`: a query,
 * case and regex toggles, the match count, previous, next and close. Enter
 * finds the next match, Shift+Enter the previous, Esc closes. It stays
 * mounted while closed, so a reopened bar keeps its query and toggles.
 */
export function TerminalSearch(props: Props) {
  const [query, setQuery] = createSignal("");
  const [caseSensitive, setCaseSensitive] = createSignal(false);
  const [regex, setRegex] = createSignal(false);
  const [results, setResults] = createSignal(NO_RESULTS);
  const request = () => {
    const text = query();
    return new TextEncoder().encode(text).byteLength > FIND_QUERY_MAX_BYTES ? null : text;
  };

  // Explicit hex highlights stay visible over any terminal theme, as Orca's do.
  const options = (incremental = false): ISearchOptions => ({
    caseSensitive: caseSensitive(),
    regex: regex(),
    incremental,
    decorations: {
      matchBackground: "#5c4a00",
      matchBorder: "#5c4a00",
      matchOverviewRuler: "#ffcc00",
      activeMatchBackground: "#c4580e",
      activeMatchBorder: "#ffcf6b",
      activeMatchColorOverviewRuler: "#ff9900",
    },
  });

  const subscription = props.addon.onDidChangeResults(setResults);
  onCleanup(() => {
    subscription.dispose();
    clearFind(props.addon);
  });

  // Each keystroke searches again from the current match. A toggle clears
  // first: the addon keeps its highlights for an unchanged query, so without
  // the clear the count would still be the old options' count.
  let toggles = "";
  createEffect(() => {
    const term = request();
    const settings = options(true);
    if (!props.open || !term) {
      clearFind(props.addon);
      setResults(NO_RESULTS);
      return;
    }
    const now = `${settings.caseSensitive} ${settings.regex}`;
    if (now !== toggles) clearFind(props.addon);
    toggles = now;
    safeFind(() => props.addon.findNext(term, settings));
  });

  const findNext = () => {
    const term = request();
    if (term) safeFind(() => props.addon.findNext(term, options()));
  };
  const findPrevious = () => {
    const term = request();
    if (term) safeFind(() => props.addon.findPrevious(term, options()));
  };

  let input!: HTMLInputElement;
  const onKeyDown = (event: KeyboardEvent) => {
    // The find chord again, from the bar: reselect the query, as Orca does.
    if (findChord(event, props.mac)) input.select();
    else if (event.key === "Escape") props.onClose();
    else if (event.key === "Enter" && event.shiftKey) findPrevious();
    else if (event.key === "Enter") findNext();
    else return;
    // Only the keys the bar answers are kept from the window; its chords
    // (Cmd+J, Cmd+W) still work from the query field.
    event.preventDefault();
    event.stopPropagation();
  };

  return (
    <Show when={props.open}>
      <div class="terminal-search" data-terminal-search-root onKeyDown={onKeyDown}>
        <input
          ref={(element) => {
            input = element;
            props.inputRef(element);
          }}
          class="terminal-search-query"
          type="text"
          placeholder="Search..."
          aria-label="Search terminal"
          value={query()}
          onInput={(event) => setQuery(event.currentTarget.value)}
        />
        <button
          type="button"
          class="terminal-search-toggle"
          title="Case sensitive"
          aria-label="Case sensitive"
          aria-pressed={caseSensitive()}
          onClick={() => setCaseSensitive((on) => !on)}
        >
          Aa
        </button>
        <button
          type="button"
          class="terminal-search-toggle"
          title="Regex"
          aria-label="Regex"
          aria-pressed={regex()}
          onClick={() => setRegex((on) => !on)}
        >
          .*
        </button>
        <span class="terminal-search-count" role="status">
          {matchStatus(request(), results())}
        </span>
        <span class="terminal-search-divider" />
        <button type="button" title="Previous match" aria-label="Previous match" onClick={findPrevious}>
          ↑
        </button>
        <button type="button" title="Next match" aria-label="Next match" onClick={findNext}>
          ↓
        </button>
        <span class="terminal-search-divider" />
        <button type="button" title="Close" aria-label="Close search" onClick={() => props.onClose()}>
          ×
        </button>
      </div>
    </Show>
  );
}
