// Source guards for wiring in `main.tsx` that no mounted test reaches: the
// module renders the whole window at import, so its call sites are checked
// here as text. Each guard names the one expression a regression would drop.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const MAIN = readFileSync(new URL("./main.tsx", import.meta.url), "utf8");

/** The body of `const <name> = ` up to the next top-level-in-Shell const. */
function body(name: string, until: string): string {
  const start = MAIN.indexOf(`const ${name} = `);
  const end = MAIN.indexOf(until, start);
  assert.ok(start >= 0 && end > start, `main.tsx still defines ${name}`);
  return MAIN.slice(start, end);
}

test("the answer banner only draws over the selected session's own terminal", () => {
  assert.ok(
    MAIN.includes("question={questionOver(question(), shownSession())}"),
    "AnswerSlot must receive the scoped question, not the bare selection's",
  );
  assert.ok(MAIN.includes('shownSessionOf(showing("terminal"), tabs(), active())'));
});

test("every tab activation, a person's or the host's, selects that tab's row", () => {
  const activate = body("activate", "const [hostAnswers");
  assert.match(activate, /const select = selectIntentFor\(tabs\(\), key\);/);
  assert.match(activate, /if \(select !== null\) dispatch\(select\);/);
});
