import type { UiStatus } from "./status.ts";
import { statusKey } from "./status.ts";
import { StatusGlyph } from "./status_glyph.tsx";
import type { Tab } from "./tabs.ts";

interface Props {
  tab: Tab;
  title: string;
  /** Whether this tab's terminal holds the work area. */
  active: boolean;
  /** Its session's status, or `null` for a bare tmux tab. */
  status: UiStatus | null;
  onChoose(): void;
  onClose(): void;
  /** A right-click on the tab: its group's menu (split, move, close). */
  onMenu?(event: MouseEvent): void;
}

/**
 * One terminal tab in the strip: its session's status glyph before the
 * title, the same glyph and words its sidebar row shows. `data-status` rides
 * on the tab too, for the tab's own tint and for tests to read, and
 * `data-key` names the tab for a drag to carry.
 */
export function TerminalTab(props: Props) {
  return (
    <span
      class="tab"
      classList={{ active: props.active }}
      data-key={props.tab.key}
      data-state={props.tab.state}
      data-status={props.status ? statusKey(props.status) : undefined}
      onContextMenu={(event) => {
        if (!props.onMenu) return;
        event.preventDefault();
        props.onMenu(event);
      }}
    >
      <button
        type="button"
        class="tab-title"
        aria-current={props.active ? "page" : undefined}
        onClick={() => props.onChoose()}
      >
        <StatusGlyph status={props.status} />
        {props.title}
      </button>
      <button type="button" class="tab-close" aria-label={`Close ${props.title}`} onClick={() => props.onClose()}>
        ×
      </button>
    </span>
  );
}
