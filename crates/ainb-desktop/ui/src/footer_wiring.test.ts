// Source guards for the status bar's counts in `main.tsx`, which renders the
// whole window at import and so cannot be mounted: both counts, and the board,
// must read the one columns memo, or a session is counted twice or late.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const MAIN = readFileSync(new URL("./main.tsx", import.meta.url), "utf8");

test("the board and both footer counts read the one columns memo", () => {
  assert.match(MAIN, /const columns = createMemo\(\(\) => boardColumns\(agentStatus\(\), fleet\(\), sessions\(\), acks\(\)\)/);
  assert.ok(MAIN.includes('needsYou={countIn(columns(), "needs")}'), "need-you is the board's Needs column");
  assert.ok(MAIN.includes('idle={countIn(columns(), "idle")}'), "idle is the board's Idle column");
  assert.ok(MAIN.includes("columns={columns()}"), "the board draws that same list");
});
