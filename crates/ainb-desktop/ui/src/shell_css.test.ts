// The stylesheet is the flat list of rules it is written as.
//
// A merge left three blocks unclosed (#46), and CSS nesting made every rule
// after the first of them a child of `.settings-page .description`: the
// review, stats, inbox and commits rules were all parsed, all valid, and all
// dead, since nothing matches under that selector. No build step said so.
// This walks the sheet's braces and refuses one that opens a rule inside
// another, or ends with a block still open.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const SHEET = join(dirname(fileURLToPath(import.meta.url)), "shell.css");

/**
 * The first line that opens a rule while another is still open, or `null`
 * for a flat sheet. Comments are dropped first, so a brace in one does not
 * count. `@media` and the like are the one legitimate nesting, so the depth
 * a rule may open at is one inside an at-rule.
 */
export function firstNestedRule(css: string): string | null {
  let depth = 0;
  let atRuleDepth = 0;
  const lines = css.replace(/\/\*[\s\S]*?\*\//g, "").split("\n");
  for (const line of lines) {
    const opens = (line.match(/{/g) ?? []).length;
    const closes = (line.match(/}/g) ?? []).length;
    if (opens > 0 && /^\s*@/.test(line)) atRuleDepth += 1;
    else if (opens > 0 && depth > atRuleDepth) return line.trim();
    depth += opens - closes;
    if (depth < atRuleDepth) atRuleDepth = depth;
  }
  return depth === 0 ? null : `${depth} block(s) still open at the end of the sheet`;
}

test("shell.css is a flat sheet: no rule opens inside another", () => {
  assert.equal(firstNestedRule(readFileSync(SHEET, "utf8")), null);
});

test("an unclosed block is caught at the rule that opens inside it", () => {
  assert.equal(firstNestedRule(".a {\n  color: red;\n.b {\n  color: blue;\n}\n"), ".b {");
  assert.equal(firstNestedRule(".a {\n  color: red;\n"), "1 block(s) still open at the end of the sheet");
  assert.equal(firstNestedRule("@media (x) {\n.a {\n  color: red;\n}\n}\n.b {\n}\n"), null);
});
