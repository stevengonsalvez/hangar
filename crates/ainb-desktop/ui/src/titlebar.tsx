import { Show } from "solid-js";

interface Props {
  /** macOS draws traffic lights over the window's own top-left corner: the
   * bar pads around them instead of drawing the app name under them. */
  mac: boolean;
  /** Open (or close) the command palette; the same action the
   * Cmd/Ctrl+Shift+K accelerator sends. */
  onSearch(): void;
  searchOpen: boolean;
  inboxOpen: boolean;
  inboxUnread: number;
  onInbox(): void;
  settingsOpen: boolean;
  onSettings(): void;
}

/**
 * The titlebar: 36px (`--h-titlebar`), `data-tauri-drag-region` so the bar
 * itself moves the window, the app name at the left and the window's three
 * global actions at the right. This replaces the old `.header`'s counts row
 * (moved to the status bar, D2) with nothing in its place: the titlebar's
 * job is chrome and navigation, not a live readout.
 *
 * Search, Inbox and Settings keep exactly the behaviour the old header's
 * buttons had; this component only draws them and forwards the click. Which
 * page is open, and what the palette does, stay `main.tsx`'s.
 */
export function Titlebar(props: Props) {
  return (
    <header class="titlebar" classList={{ mac: props.mac }} data-tauri-drag-region="true">
      <span class="titlebar-app">ainb</span>
      <div class="titlebar-actions">
        <button
          type="button"
          class="titlebar-search"
          title="Search"
          aria-label="Search"
          aria-pressed={props.searchOpen}
          onClick={() => props.onSearch()}
        >
          Search
        </button>
        <button
          type="button"
          class="inbox-button"
          title="Inbox"
          aria-pressed={props.inboxOpen}
          onClick={() => props.onInbox()}
        >
          Inbox
          <Show when={props.inboxUnread > 0}>
            <span class="inbox-unread">{props.inboxUnread}</span>
          </Show>
        </button>
        <button
          type="button"
          class="settings"
          title="Settings"
          aria-pressed={props.settingsOpen}
          onClick={() => props.onSettings()}
        >
          ⚙
        </button>
      </div>
    </header>
  );
}
