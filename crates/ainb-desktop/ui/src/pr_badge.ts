// A worktree card's PR badge, as words: what the host's `pr_badge` answer
// says, and what the badge reads aloud. The host reads the PR through `gh`,
// caches it and answers `null` for every miss (`pr_badge.rs`); this only
// draws what came back.

import type { PrChecks, PrBadge, PrState } from "../../bindings/Desktop.ts";

export type { PrChecks, PrBadge, PrState };

/** How often a mounted card asks again. The host's cache (a 120 s TTL)
 * decides whether `gh` actually runs, so this only bounds how late a fresh
 * answer is drawn. */
export const PR_BADGE_REFRESH_MS = 60_000;

const STATES: Record<PrState, string> = { open: "Open", draft: "Draft", merged: "Merged", closed: "Closed" };
const CHECKS: Record<PrChecks, string | null> = {
  pass: "checks passing",
  fail: "checks failing",
  pending: "checks running",
  none: null,
};

/** `answer` as a badge, or `null` when it is not one this build draws: an
 * older host, a test fixture, or a miss. Nothing is thrown for it. */
export function asBadge(answer: unknown): PrBadge | null {
  if (typeof answer !== "object" || answer === null) return null;
  const { state, number, url, checks } = answer as Record<string, unknown>;
  const known =
    typeof state === "string" &&
    state in STATES &&
    typeof checks === "string" &&
    checks in CHECKS &&
    Number.isInteger(number) &&
    (number as number) > 0 &&
    typeof url === "string";
  return known ? (answer as PrBadge) : null;
}

/** The badge's state word: Open, Draft, Merged or Closed. */
export const stateLabel = (badge: PrBadge): string => STATES[badge.state];

/** What the badge reads aloud and shows on hover: `PR #12 open, checks
 * failing`, without the checks when there are none. */
export function badgeTitle(badge: PrBadge): string {
  const checks = CHECKS[badge.checks];
  const title = `PR #${badge.number} ${stateLabel(badge).toLowerCase()}`;
  return checks === null ? title : `${title}, ${checks}`;
}
