// The whole window, mounted over a fake host: a worktree card whose branch
// has a PR draws its badge and CI dot, a click on the badge opens the PR
// through the host's `open_url` and never opens or selects a row, and a card
// whose lookup missed, or whose command failed, draws no badge at all, even
// one that drew a badge before its refresh missed. A right-click on the badge
// opens no row menu.

import "./window.ts";
import { drain, host, mountWindow, until } from "./window_host.ts";

import assert from "node:assert/strict";
import { before, mock, test } from "node:test";
import { PR_BADGE_REFRESH_MS } from "../../src/pr_badge.ts";

const URL = "https://github.com/o/r/pull/42";
const BADGE = { state: "open", number: 42, url: URL, checks: "fail" };

before(async () => {
  // `u-1` has an open PR with failing CI; `u-2` has none (the host's miss is
  // `null`); `u-3`'s command fails outright.
  host.prBadges.set("u-1", BADGE);
  host.prBadges.set("u-3", new Error("state not managed"));
  // Only the badge's refresh clock: the window's own waits keep real time.
  mock.timers.enable({ apis: ["setInterval"] });
  await mountWindow();
});

/** The worktree card holding `session`'s row. */
const cardOf = (session: string) =>
  document.querySelector(`.session-row[data-session="${session}"]`)?.closest<HTMLElement>(".worktree-card") ?? null;
const badgeOf = (session: string) => cardOf(session)?.querySelector<HTMLElement>(".pr-badge") ?? null;
// Booleans, never elements, in an assertion: a failed one prints its actual
// value, and a happy-dom node is a walk of the whole window.
const hasBadge = (session: string) => badgeOf(session) !== null;
const hasCard = (session: string) => cardOf(session) !== null;

test("a card with an open PR and failing CI draws the badge and a fail dot", async () => {
  await until(() => hasBadge("u-1"), "u-1's PR badge");
  const badge = badgeOf("u-1")!;
  assert.equal(badge.dataset.state, "open");
  assert.equal(badge.querySelector(".pr-state")?.textContent, "Open");
  assert.equal(badge.querySelector(".pr-number")?.textContent, "#42");
  assert.equal(badge.querySelector(".ci-dot")?.getAttribute("data-checks"), "fail");
  assert.equal(badge.getAttribute("aria-label"), "Open #42, pull request, checks failing");
  assert.equal(badge.closest(".worktree-card-meta") !== null, true, "on the card's meta line");
  assert.equal(badge.closest(".session-row") !== null, false, "outside every row's button");
});

test("a card whose lookup missed or failed draws no badge", async () => {
  await until(() => host.prAsked.includes("u-2") && host.prAsked.includes("u-3"), "every card asked");
  await drain();
  assert.equal(hasCard("u-2"), true, "u-2's card is drawn");
  assert.equal(hasBadge("u-2"), false, "a miss draws nothing");
  assert.equal(hasCard("u-3"), true, "u-3's card is drawn");
  assert.equal(hasBadge("u-3"), false, "a failed command draws nothing");
  assert.equal(document.querySelectorAll(".pr-badge").length, 1);
});

test("clicking the badge opens the PR through open_url and does not open or select the row", async () => {
  await until(() => hasBadge("u-1"), "u-1's PR badge");
  const sent = host.sent.length;
  const selected = host.selected;
  // Nothing around the badge acts on a click today, so what proves the click
  // stops there is that the card never hears it.
  let cardHeard = 0;
  cardOf("u-1")!.addEventListener("click", () => (cardHeard += 1));
  const clicked = new window.MouseEvent("click", { bubbles: true, cancelable: true });
  badgeOf("u-1")!.dispatchEvent(clicked);
  await drain();
  assert.deepEqual(host.opened, [URL]);
  assert.deepEqual(host.sent.slice(sent), [], "no intent reached the host");
  assert.equal(host.selected, selected, "the selection did not move");
  assert.equal(document.querySelector(".session-row[aria-current]") !== null, false, "no row is drawn selected");
  assert.equal(document.activeElement?.closest(".session-row") != null, false, "no row took the keyboard");
  assert.equal(cardHeard, 0, "the click stopped at the badge");
});

test("right-clicking the badge opens no row menu, while the card's row still has one", async () => {
  await until(() => hasBadge("u-1"), "u-1's PR badge");
  const rightClick = (target: Element) =>
    target.dispatchEvent(new window.MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 5, clientY: 5 }));
  rightClick(badgeOf("u-1")!);
  await drain();
  assert.equal(document.querySelector(".row-menu") !== null, false, "no menu on the badge");
  rightClick(cardOf("u-1")!.querySelector(".session-row")!);
  await until(() => document.querySelector(".row-menu") !== null, "the row's own menu");
  (document.activeElement ?? document.body).dispatchEvent(new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  await until(() => document.querySelector(".row-menu") === null, "the menu closed");
});

test("a card whose refresh misses or fails drops the badge it drew", async () => {
  await until(() => hasBadge("u-1"), "u-1's PR badge");
  const refresh = async () => {
    const asked = host.prAsked.length;
    mock.timers.tick(PR_BADGE_REFRESH_MS);
    await until(() => host.prAsked.length > asked, "a refresh");
    await drain();
  };
  host.prBadges.delete("u-1");
  await refresh();
  assert.equal(hasBadge("u-1"), false, "a miss after a badge draws nothing");
  host.prBadges.set("u-1", { ...BADGE, checks: "pass" });
  await refresh();
  assert.equal(badgeOf("u-1")?.querySelector(".ci-dot")?.getAttribute("data-checks"), "pass", "a later answer draws again");
  host.prBadges.set("u-1", new Error("gone"));
  await refresh();
  assert.equal(hasBadge("u-1"), false, "a failed command after a badge draws nothing");
});
