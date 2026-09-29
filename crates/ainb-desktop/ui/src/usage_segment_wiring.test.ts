// Source guards for the usage segment's one subscription: `main.tsx` renders
// the whole window at import and cannot be mounted, so its wiring is checked
// here as text. Section 21 is subscribed once and read once, the stats tab and
// the status bar segment draw that one read, and the segment's own modules
// reach neither the host nor the store, so it cannot grow a second
// subscription or a poll of its own.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { SUBSCRIBED } from "./subscription.ts";

const source = (name: string) => readFileSync(new URL(name, import.meta.url), "utf8");
const MAIN = source("./main.tsx");

test("section 21 is subscribed once and read once, by the window", () => {
  assert.equal(SUBSCRIBED.filter((name) => name === "usage").length, 1);
  assert.equal(MAIN.match(/shellUsage\(/g)?.length, 1, "one read of section 21 in main.tsx");
  assert.ok(MAIN.includes("const usage = () => shellUsage(store, host());"));
});

test("the stats tab and the status bar segment draw that same read", () => {
  assert.ok(MAIN.includes("<Stats usage={usage()} stale={usageStale()} />"));
  assert.match(MAIN, /<UsageSegment\s+usage=\{usage\(\)\}\s+stale=\{usageStale\(\)\}/);
});

test("a click on the segment does what the Stats tab's own click does", () => {
  const segment = MAIN.slice(MAIN.indexOf("<UsageSegment"), MAIN.indexOf("/>", MAIN.indexOf("<UsageSegment")));
  assert.match(segment, /onOpen=\{\(\) => \{\s*closeTranscript\(\);\s*setPane\("stats"\);\s*\}\}/);
});

test("the segment's modules cannot subscribe, read the store or poll", () => {
  for (const file of ["./usage_segment.ts", "./usage_segment.tsx"]) {
    const text = source(file);
    for (const banned of ["@tauri-apps", "invoke", "listen(", "./store", "./subscription", "setInterval", "setTimeout"]) {
      assert.ok(!text.includes(banned), `${file} must not use ${banned}: it draws the window's read`);
    }
  }
});
