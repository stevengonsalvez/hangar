// The status bar's usage segment says what Orca's provider segment says, from
// section 21: a loading mark, "--" when there is no summary to have, a failed
// read as a warning, and otherwise one chip per provider with the stale mark
// when the numbers are not the daemon's latest complete word.

import assert from "node:assert/strict";
import { test } from "node:test";
import type { UsageBucketFrame, UsageSummaryFrame, UsageView } from "../../../ainb-app/bindings/AppState";
import { chipFigure, compactTokens, MAX_CHIPS, usageSegment } from "./usage_segment.ts";

const bucket = (input: number, cost: number | null = null): UsageBucketFrame => ({
  input_tokens: input,
  cache_creation_tokens: 0,
  cache_read_tokens: 0,
  output_tokens: 0,
  reasoning_tokens: 0,
  call_count: 1,
  session_count: 1,
  project_count: 1,
  cost_usd: cost,
});

const summary = (over: Partial<UsageSummaryFrame> = {}): UsageSummaryFrame => ({
  state: "ready",
  totals: bucket(3000, 15),
  daily: [],
  daily_cut: 0,
  providers: [
    { name: "claude", bucket: bucket(2000, 12.5) },
    { name: "codex", bucket: bucket(1000, 2.5) },
  ],
  providers_cut: 0,
  models: [],
  models_cut: 0,
  projects: [],
  projects_cut: 0,
  detail: null,
  ...over,
});

const view = (over: Partial<UsageView> = {}): UsageView => ({
  absent: null,
  failure: null,
  summary: summary(),
  ...over,
});

test("a priced provider reads as its cost, an unpriced one as its tokens, never $0", () => {
  assert.equal(chipFigure(bucket(10, 12.5)), "$12.50");
  assert.equal(chipFigure(bucket(10, 0)), "$0.00");
  assert.equal(chipFigure(bucket(1_234_567, null)), "1.2M tok");
});

test("token counts are compact enough for a 24px bar", () => {
  assert.equal(compactTokens(0), "0");
  assert.equal(compactTokens(950), "950");
  assert.equal(compactTokens(12_345), "12.3K");
  assert.equal(compactTokens(999_950), "1M", "rounding carries into the next unit");
  assert.equal(compactTokens(4_200_000_000), "4.2B");
});

test("before the host frames section 21, and while the daemon scans, the segment is loading", () => {
  assert.equal(usageSegment(undefined).state, "loading");
  assert.equal(usageSegment(view({ summary: null })).state, "loading");
  const scanning = usageSegment(view({ summary: summary({ state: "scanning", totals: null }) }));
  assert.equal(scanning.state, "loading");
  assert.deepEqual(scanning.chips, [], "scanning has counted nothing: no numbers");
  assert.equal(scanning.title, "The daemon is still scanning provider logs");
});

test("no summary to have is unavailable, with the host's reason as the tooltip", () => {
  const absent = usageSegment(view({ summary: null, absent: "the daemon does not serve fleet.usage.read" }));
  assert.equal(absent.state, "unavailable");
  assert.equal(absent.title, "the daemon does not serve fleet.usage.read");
  const cannot = usageSegment(view({ summary: summary({ state: "unavailable", totals: null, providers: [] }) }));
  assert.equal(cannot.state, "unavailable");
});

test("a failed read with nothing held is a failure, not a zero", () => {
  const failed = usageSegment(view({ summary: null, failure: "socket closed" }));
  assert.equal(failed.state, "failed");
  assert.deepEqual(failed.chips, []);
  assert.equal(failed.title, "The daemon could not be read: socket closed");
});

test("a ready summary draws one chip per provider, in the daemon's order, not stale", () => {
  const ready = usageSegment(view());
  assert.equal(ready.state, "ready");
  assert.deepEqual(ready.chips, [
    { provider: "claude", figure: "$12.50" },
    { provider: "codex", figure: "$2.50" },
  ]);
  assert.equal(ready.more, 0);
  assert.equal(ready.stale, false);
  assert.equal(ready.title, "Usage over the trailing 30 days");
});

test("numbers that are not the daemon's latest complete word carry the stale mark", () => {
  assert.equal(usageSegment(view({ failure: "timed out" })).stale, true, "a failed refresh over held numbers");
  assert.equal(usageSegment(view({ summary: summary({ state: "partial" }) })).stale, true);
  assert.equal(usageSegment(view(), true).stale, true, "the host withheld the section as oversize");
  assert.match(usageSegment(view({ failure: "timed out" })).title, /The last read failed: timed out/);
});

test("past the cap, the rest fold into one +N that names them, cut providers counted", () => {
  const providers = ["claude", "codex", "cursor", "gemini", "grok"].map((name, n) => ({
    name,
    bucket: bucket(100, n + 1),
  }));
  const many = usageSegment(view({ summary: summary({ providers, providers_cut: 2 }) }));
  assert.equal(many.chips.length, MAX_CHIPS);
  assert.equal(many.more, 4, "two over the cap plus two the frame cut");
  assert.equal(many.moreTitle, "Also: gemini $4.00, grok $5.00, 2 not sent");
});

test("a ready summary with no provider used is empty, drawn as such", () => {
  const empty = usageSegment(view({ summary: summary({ providers: [] }) }));
  assert.equal(empty.state, "empty");
  assert.deepEqual(empty.chips, []);
});

test("a frame that cut every provider is not empty: +N counts them, as the stats tab does", () => {
  const cut = usageSegment(view({ summary: summary({ providers: [], providers_cut: 4 }) }));
  assert.equal(cut.state, "ready");
  assert.deepEqual(cut.chips, []);
  assert.equal(cut.more, 4);
  assert.equal(cut.moreTitle, "Also: 4 not sent");
});

test("an empty or unavailable summary still carries the stale mark of a failed refresh", () => {
  assert.equal(usageSegment(view({ summary: summary({ providers: [] }), failure: "timed out" })).stale, true);
  assert.equal(usageSegment(view({ summary: summary({ providers: [] }) }), true).stale, true);
  assert.equal(usageSegment(view({ summary: summary({ providers: [] }) })).stale, false);
});
