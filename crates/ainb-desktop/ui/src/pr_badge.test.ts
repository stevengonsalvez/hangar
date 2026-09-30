import assert from "node:assert/strict";
import { test } from "node:test";
import { asBadge, badgeTitle, stateLabel } from "./pr_badge.ts";

const badge = { state: "draft", number: 7, url: "https://github.com/o/r/pull/7", checks: "pending" } as const;

test("a host answer is a badge only when every field is one this build draws", () => {
  assert.deepEqual(asBadge(badge), badge);
  for (const answer of [
    null,
    undefined,
    "open",
    {},
    { ...badge, state: "reopened" },
    { ...badge, checks: "neutral" },
    { ...badge, number: 0 },
    { ...badge, number: 1.5 },
    { ...badge, number: "7" },
    { ...badge, url: 7 },
  ]) {
    assert.equal(asBadge(answer), null, JSON.stringify(answer));
  }
});

test("the badge reads its state, number and checks, and leaves out checks it has none of", () => {
  assert.equal(stateLabel(badge), "Draft");
  assert.equal(badgeTitle(badge), "PR #7 draft, checks running");
  assert.equal(badgeTitle({ ...badge, state: "merged", checks: "pass" }), "PR #7 merged, checks passing");
  assert.equal(badgeTitle({ ...badge, state: "closed", checks: "none" }), "PR #7 closed");
});
