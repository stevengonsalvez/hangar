import { For, onCleanup, onMount, Show } from "solid-js";

/** A row of a terminal's context menu. */
export interface MenuEntry {
  label: string;
  /** The chord that does the same, when there is one. */
  shortcut?: string;
  run(): void;
}

/** Where a menu opens, in viewport pixels. */
export interface MenuPoint {
  x: number;
  y: number;
}

/**
 * Whether `event` opens the menu from the keyboard: the context-menu key, or
 * Shift+F10, the chord every desktop toolkit gives it.
 */
export function menuChord(event: {
  key: string;
  shiftKey: boolean;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
}): boolean {
  if (event.metaKey || event.ctrlKey || event.altKey) return false;
  return event.key === "ContextMenu" || (event.key === "F10" && event.shiftKey);
}

interface Props {
  /** Where the menu is open, or `null` while it is closed. */
  at: MenuPoint | null;
  entries: MenuEntry[];
  /**
   * The menu wants to close. `refocus` is true when the keyboard goes back to
   * the terminal (Esc, Tab, a chosen row), false for a click or a focus
   * elsewhere, which keeps the focus it gave.
   */
  onClose(refocus: boolean): void;
}

/**
 * A terminal's right-click menu, ported from Orca's `TerminalContextMenu.tsx`:
 * the rows it is given, in order, at the pointer. Arrows walk the rows, Enter
 * or Space runs one, Esc closes. A chosen row closes the menu, handing the
 * keyboard back to the terminal, before it runs, so a row that moves focus
 * (Find) keeps it.
 */
export function TerminalMenu(props: Props) {
  return (
    <Show when={props.at} keyed>
      {(at) => <OpenMenu at={at} entries={props.entries} onClose={(refocus) => props.onClose(refocus)} />}
    </Show>
  );
}

function OpenMenu(props: { at: MenuPoint; entries: MenuEntry[]; onClose(refocus: boolean): void }) {
  let menu!: HTMLDivElement;
  const rows = () => [...menu.querySelectorAll<HTMLButtonElement>('[role="menuitem"]')];
  const step = (by: number) => {
    const all = rows();
    const at = all.indexOf(document.activeElement as HTMLButtonElement);
    // With no row focused, down starts at the first and up at the last.
    const next = at < 0 ? (by > 0 ? 0 : all.length - 1) : (at + by + all.length) % all.length;
    all[next]?.focus();
  };
  const choose = (entry: MenuEntry) => {
    props.onClose(true);
    entry.run();
  };

  onMount(() => {
    // Kept inside the window: a click near the right or bottom edge opens the
    // menu up or left of the pointer instead of past the edge.
    const box = menu.getBoundingClientRect();
    if (props.at.x + box.width > window.innerWidth) menu.style.left = `${Math.max(0, window.innerWidth - box.width)}px`;
    if (props.at.y + box.height > window.innerHeight) menu.style.top = `${Math.max(0, props.at.y - box.height)}px`;
    rows()[0]?.focus();

    const outside = (event: Event) => {
      if (!menu.contains(event.target as Node)) props.onClose(false);
    };
    // The window losing focus, or changing size: closed, and the terminal
    // has the keyboard again when the window is back.
    const away = () => props.onClose(true);
    document.addEventListener("pointerdown", outside, true);
    window.addEventListener("blur", away);
    window.addEventListener("resize", away);
    onCleanup(() => {
      document.removeEventListener("pointerdown", outside, true);
      window.removeEventListener("blur", away);
      window.removeEventListener("resize", away);
    });
  });

  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === "ArrowDown") step(1);
    else if (event.key === "ArrowUp") step(-1);
    else if (event.key === "Home") rows()[0]?.focus();
    else if (event.key === "End") rows().at(-1)?.focus();
    else if (event.key === "Escape" || event.key === "Tab") props.onClose(true);
    else if (event.key === "Enter" || event.key === " ") (document.activeElement as HTMLButtonElement | null)?.click();
    else return;
    // The keys the menu answers stay with it: Esc must not reach the window's
    // own Esc handling, and Enter must not also press the row natively.
    event.preventDefault();
    event.stopPropagation();
  };

  return (
    <div
      ref={menu}
      class="terminal-menu"
      role="menu"
      aria-label="Terminal"
      style={{ left: `${props.at.x}px`, top: `${props.at.y}px` }}
      onKeyDown={onKeyDown}
      // Focus moving elsewhere (a chord that opens the palette) closes it; a
      // null target is the window's blur, answered above.
      onFocusOut={(event) => {
        const to = event.relatedTarget as Node | null;
        if (to && !menu.contains(to)) props.onClose(false);
      }}
      // A press on the padding keeps focus on the row it is on.
      onMouseDown={(event) => event.preventDefault()}
      // The webview's own menu never opens over this one.
      onContextMenu={(event) => event.preventDefault()}
    >
      <For each={props.entries}>
        {(entry) => (
          <button
            type="button"
            role="menuitem"
            tabIndex={-1}
            // The pointer moves the focus, as Radix's rows do, so one row is lit.
            onPointerMove={(event) => event.currentTarget.focus()}
            onClick={() => choose(entry)}
          >
            <span>{entry.label}</span>
            <Show when={entry.shortcut}>
              <kbd>{entry.shortcut}</kbd>
            </Show>
          </button>
        )}
      </For>
    </div>
  );
}
