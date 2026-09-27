import { Show } from "solid-js";
import { statusKey, statusLabel, type UiStatus } from "./status.ts";

/**
 * One status, drawn the same way on a sidebar row and a tab: a shape AND a
 * colour (a question mark, a check, a cross, a spinner, a dashed ring, a
 * dot), never colour alone, since needs-you amber and done mint are close in
 * luminance. The shape is decorative (`aria-hidden`); the words go in a
 * visually hidden label inside the same control, so a screen reader hears
 * "Needs you · approve" as part of the row or tab's own name.
 */
export function StatusGlyph(props: { status: UiStatus | null }) {
  return (
    <Show when={props.status}>
      {(status) => (
        <>
          <span class="status-glyph" data-status={statusKey(status())} aria-hidden="true" />
          {/* The trailing separator keeps the words apart from the name that
              follows, so a screen reader hears "Working, api", not
              "Workingapi". */}
          <span class="visually-hidden">{`${statusLabel(status())}, `}</span>
        </>
      )}
    </Show>
  );
}
