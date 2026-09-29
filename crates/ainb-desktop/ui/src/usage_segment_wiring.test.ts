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

/** The body of the first `<attr>={() => { ... }}` handler after `from`, whitespace folded. */
function handler(from: string, attr: string): string {
  const start = MAIN.indexOf(from);
  assert.ok(start >= 0, `main.tsx still has ${from}`);
  const open = MAIN.indexOf(`${attr}={() => {`, start);
  const close = MAIN.indexOf("}}", open);
  assert.ok(open > start && close > open, `${from} still has an ${attr} handler`);
  return MAIN.slice(open + `${attr}={() => {`.length, close).replace(/\s+/g, " ").trim();
}

test("a click on the segment does exactly what the Stats tab's own click does", () => {
  const tab = handler('class="tab stats-tab"', "onClick");
  assert.equal(tab, 'closeTranscript(); setPane("stats");', "the Stats tab's click");
  assert.equal(handler("<UsageSegment", "onOpen"), tab);
});

// Anything that could subscribe, read the store or poll comes in by import;
// the segment may import only its projection, Solid and the wire types.
const ALLOWED = new Set(["solid-js", "./stats.ts", "./usage_segment.ts", "../../../ainb-app/bindings/AppState"]);

test("the segment's modules import nothing that could subscribe, read the store or poll", () => {
  for (const file of ["./usage_segment.ts", "./usage_segment.tsx"]) {
    const text = source(file);
    const imports = [...text.matchAll(/^import[^;]*?from "([^"]+)";/gms)].map(([, from]) => from);
    assert.ok(imports.length > 0, `${file}: the import scan found its imports`);
    for (const from of imports) {
      assert.ok(ALLOWED.has(from), `${file} imports ${from}: it must draw the window's read`);
    }
    assert.doesNotMatch(text, /\bimport\(/, `${file} must not import dynamically`);
    for (const banned of ["createResource", "fetch(", "setInterval", "setTimeout", "requestAnimationFrame", "__TAURI"]) {
      assert.ok(!text.includes(banned), `${file} must not use ${banned}: it draws the window's read`);
    }
  }
});
