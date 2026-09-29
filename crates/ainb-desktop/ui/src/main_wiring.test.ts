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
  // After leaving Settings, in the same ordered run (`test/dom/settings_tab.test.ts`).
  assert.match(activate, /void run\(\[\.\.\.leave, \.\.\.first, \.\.\.\(select === null \? \[\] : \[select\]\)\]\);/);
});

test("the create flow reads the window's session list and selects through the reducer", () => {
  assert.ok(MAIN.includes("sessions: () => sessions(),"), "the flow must see the frames the window draws");
  assert.match(MAIN, /select: \(sessionId\) => dispatch\(selectRowIntent\(\{ session: sessionId \}\)\)/);
});

test("the answer banner is remounted when the shown terminal changes", () => {
  // Keyed by the shown session, so a latched banner does not hold the last
  // session's question over the next pane for its grace.
  assert.match(MAIN, /<For each=\{\[shownSession\(\)\]\}>\s*\{\(\) => <AnswerSlot /);
});

test("closing the shown tab shows the tab Orca would, not the strip's first", () => {
  const showTabs = body("showTabs", "const report = ");
  assert.match(showTabs, /tabAfterClose\(before, view\.tabs, recent, active\(\)\)/);
  assert.doesNotMatch(showTabs, /view\.tabs\[0\]/);
  const activate = body("activate", "const [hostAnswers");
  assert.match(activate, /recent = visited\(recent, key\);/, "every shown tab is remembered");
});
