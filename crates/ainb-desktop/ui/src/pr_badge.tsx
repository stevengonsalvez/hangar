import { createEffect, createMemo, createSignal, on, onCleanup, Show } from "solid-js";
import { asBadge, badgeTitle, PR_BADGE_REFRESH_MS, stateLabel, type PrBadge } from "./pr_badge.ts";

/**
 * A worktree card's PR badge: the PR's state and number, and a CI dot while
 * it has checks. Asks the host for `sessionId`'s badge on mount, whenever the
 * card's session or branch changes, and every `PR_BADGE_REFRESH_MS` while the
 * window is visible; draws nothing until an answer is a badge, and nothing
 * again after a miss.
 *
 * A click opens the PR through `onOpenUrl` (the host's `open_url`, whose
 * rule the URL passes) and goes no further: the card around the badge never
 * hears it, so it never opens or selects a row.
 */
export function PrBadgeButton(props: {
  sessionId: string;
  branch: string;
  fetch(sessionId: string): Promise<unknown>;
  onOpenUrl(url: string): void;
}) {
  const [badge, setBadge] = createSignal<PrBadge | null>(null);
  // A string, so a frame that rebuilds the card with the same session and
  // branch does not ask again.
  const asked = createMemo(() => `${props.sessionId}\n${props.branch}`);
  createEffect(
    on(asked, () => {
      const sessionId = props.sessionId;
      let live = true;
      setBadge(null);
      const ask = () =>
        props.fetch(sessionId).then(
          (answer) => live && setBadge(asBadge(answer)),
          () => live && setBadge(null),
        );
      void ask();
      // A hidden window has nobody to show a fresher badge to.
      const timer = setInterval(() => document.hidden || void ask(), PR_BADGE_REFRESH_MS);
      // Under Node (the DOM tests mount the whole window and never unmount
      // it) a live timer would hold the test process open; a browser's timer
      // id has no `unref`.
      (timer as { unref?(): void }).unref?.();
      onCleanup(() => {
        live = false;
        clearInterval(timer);
      });
    }),
  );
  return (
    <Show when={badge()}>
      {(shown) => (
        <button
          type="button"
          class="pr-badge"
          data-state={shown().state}
          data-checks={shown().checks}
          title={badgeTitle(shown())}
          aria-label={badgeTitle(shown())}
          // A native listener, not Solid's delegated `onClick`: a delegated
          // handler runs at the document, after the card and every other
          // ancestor has already heard the click.
          on:click={(event: MouseEvent) => {
            event.preventDefault();
            event.stopPropagation();
            props.onOpenUrl(shown().url);
          }}
        >
          <span class="pr-state">{stateLabel(shown())}</span>
          <span class="pr-number">#{shown().number}</span>
          <Show when={shown().checks !== "none"}>
            <span class="ci-dot" data-checks={shown().checks} aria-hidden="true" />
          </Show>
        </button>
      )}
    </Show>
  );
}
