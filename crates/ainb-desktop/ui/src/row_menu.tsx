import { For, onCleanup, onMount } from "solid-js";
import { stepItem, type RowMenuAction, type RowMenuItem } from "./row_menu.ts";

interface Props {
  /** Where the menu's top-left corner goes, in viewport pixels. */
  x: number;
  y: number;
  /** The menu's accessible name: which row it acts on. */
  label: string;
  items: RowMenuItem[];
  onPick(action: RowMenuAction): void;
  /** Esc, Tab, or a press outside: close without choosing. */
  onClose(): void;
}

/**
 * A row's context menu, the way Orca's behaves (a Radix dropdown in
 * `WorktreeContextMenuView.tsx:134-156`): the first enabled item takes the
 * keyboard, the arrows walk the enabled items and wrap, Home and End jump,
 * Esc or Tab closes it. The caller mounts one per opening and puts the
 * keyboard back on the row when it closes.
 */
export function RowMenu(props: Props) {
  let menu!: HTMLDivElement;
  const buttons: HTMLButtonElement[] = [];
  const focusAt = (index: number) => {
    if (index >= 0) buttons[index]?.focus();
  };
  const focused = () => buttons.indexOf(document.activeElement as HTMLButtonElement);

  onMount(() => {
    // Kept inside the window: a right-click near the right or bottom edge
    // would otherwise draw half the menu off screen.
    const rect = menu.getBoundingClientRect();
    menu.style.left = `${Math.max(0, Math.min(props.x, window.innerWidth - rect.width))}px`;
    menu.style.top = `${Math.max(0, Math.min(props.y, window.innerHeight - rect.height))}px`;
    focusAt(stepItem(props.items, -1, 1));
    // Capture, so a press that something else stops still closes the menu.
    const outside = (event: Event) => {
      if (!menu.contains(event.target as Node)) props.onClose();
    };
    document.addEventListener("pointerdown", outside, true);
    onCleanup(() => document.removeEventListener("pointerdown", outside, true));
  });

  const onKeyDown = (event: KeyboardEvent) => {
    switch (event.key) {
      case "ArrowDown":
      case "ArrowUp":
        focusAt(stepItem(props.items, focused(), event.key === "ArrowDown" ? 1 : -1));
        break;
      case "Home":
        focusAt(stepItem(props.items, -1, 1));
        break;
      case "End":
        focusAt(stepItem(props.items, -1, -1));
        break;
      case "Escape":
      case "Tab":
        props.onClose();
        break;
      default:
        return;
    }
    event.preventDefault();
    event.stopPropagation();
  };

  return (
    <div
      ref={menu}
      class="row-menu"
      role="menu"
      aria-label={props.label}
      style={{ left: `${props.x}px`, top: `${props.y}px` }}
      onKeyDown={onKeyDown}
      // A right-click on the open menu is not a second menu over it.
      onContextMenu={(event) => event.preventDefault()}
    >
      <For each={props.items}>
        {(item, index) => (
          <button
            ref={(element) => (buttons[index()] = element)}
            type="button"
            role="menuitem"
            class="row-menu-item"
            tabIndex={-1}
            disabled={item.disabled}
            title={item.reason}
            onClick={() => {
              if (!item.disabled) props.onPick(item.action);
            }}
          >
            {item.label}
          </button>
        )}
      </For>
    </div>
  );
}
