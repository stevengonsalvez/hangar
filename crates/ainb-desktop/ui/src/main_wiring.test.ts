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
  // Only once the host has left any page (`test/dom/settings_tab.test.ts`).
  assert.match(
    activate,
    /void invoke\("answer_home"\)\.then\(\(\) => run\(\[\.\.\.first, \.\.\.\(select === null \? \[\] : \[select\]\)\]\)\);/,
  );
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

test("closing the shown tab moves the sidebar to the tab shown next", () => {
  // Else the selection stays on the closed tab's session, and `questionOver`
  // hides the new terminal's own question as another session's. Not under
  // Settings or the Inbox, whose screen refuses the row
  // (`test/dom/settings_tab.test.ts`).
  const showTabs = body("showTabs", "const report = ");
  assert.match(
    showTabs,
    /if \(next !== null && pane\(\) === "terminal"\) \{[^}]*if \(!settings\(\) && !inboxOpen\(\)\) \{\s*const select = selectIntentFor\(view\.tabs, next\);\s*if \(select !== null\) void invoke<Refusal \| null>\("dispatch", \{ intent: select \}\);/,
  );
});

test("a sidebar row's menu runs through the window's own open, ordered dispatch and clipboard", () => {
  // `test/dom/sidebar_menu.test.ts` proves the menu hands `runRowPick` the
  // right pick; this is the one place those deps become real host calls.
  assert.match(
    MAIN,
    /onRowPick=\{\(pick\) =>\s*runRowPick\(pick, \{\s*open: openSession,\s*run: \(intents\) => void run\(intents\),\s*copy: \(text\) => void invoke\("clipboard_write", \{ text \}\),\s*reselect: shownRowIntents,\s*\}\)\s*\}/,
  );
  // The row that goes back is the shown terminal's, and only while one is shown.
  const shown = body("shownRowIntents", "onMount(");
  assert.match(shown, /showing\("terminal"\) && key !== null \? selectIntentFor\(tabs\(\), key\) : null/);
});
