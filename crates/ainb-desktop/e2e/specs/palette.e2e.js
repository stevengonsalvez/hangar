// The palette over a terminal (#47): with a terminal tab focused, the chord
// opens the palette and every keystroke after it is the palette's, whether
// the tab had focus for a while or was still being opened when the chord
// landed. What the pane read is checked from the pane itself, so a leaked
// keystroke is caught where it does damage rather than where it was typed.

import assert from "node:assert/strict";
import { click, setPaletteQuery } from "../support.js";
import { hook, paneText, seeded } from "../world.js";

/** The shell accelerator, as this platform spells it. */
const MOD = process.platform === "darwin" ? ["Meta"] : ["Control", "Shift"];

/** Typed into the palette, never into the pane. */
const QUERY = "stats";

/** A screenshot into the directory `AINB_E2E_SHOTS` names, when it names one. */
async function shot(name) {
  const dir = process.env.AINB_E2E_SHOTS;
  if (dir) await browser.saveScreenshot(`${dir}/${name}.png`);
}

/** The element that has keyboard focus, by class. */
const focused = () => browser.execute(() => document.activeElement?.className ?? "");

/** Where the keyboard is and whether the page is on screen, for a failure to say. */
async function focusState() {
  return browser.execute(() => ({
    active: `${document.activeElement?.tagName}.${document.activeElement?.className}`,
    visibility: document.visibilityState,
    tabs: document.querySelectorAll(".tab[data-state]").length,
  }));
}

/** How many lines the agent in `session`'s pane has read so far. */
const linesRead = (session) => (paneText(session.tmux).match(/agent read:/g) ?? []).length;

/**
 * Give `session`'s terminal the keyboard the way a person does: choose its
 * row, then click its pane. The shell also focuses a tab it has just opened,
 * but from an animation frame, and a page the runner keeps off screen (xvfb,
 * an occluded window) never runs one, so a wait on that alone holds on one
 * runner and not another. The keyboard is then proven where it matters: an
 * Enter through the driver reaches the agent, which echoes the line it read.
 * `document.activeElement` is not read for it, because WebKitGTK under xvfb
 * never names the terminal's textarea there even while its keys land.
 */
async function focusTerminal(session) {
  await click(`.session-row[data-session="${session.id}"]`);
  // This session's own tab, by its key: another spec in the world may have
  // left its tab mounted, hidden, ahead of this one in the document.
  const tab = `.terminal[data-tab="${session.tmux}"]`;
  await $(tab).waitForExist({ timeout: 60_000 });
  // The click and the focus behind it take a few tries on the Linux runner,
  // whose driver hands the page its keys unevenly; each try is proven by a
  // line the agent read, and the last failure says where the keyboard was.
  for (let attempt = 1; attempt <= 6; attempt += 1) {
    await click(`${tab} .xterm`);
    // A token through the terminal's own input event, and Enter through the
    // driver, exactly as the journey spec types on both runners: the token
    // read back from the pane is the keyboard, proven.
    const token = `focus-${Date.now()}-${attempt}`;
    await browser.execute(
      (selector, text) => {
        const area = document.querySelector(`${selector} .xterm-helper-textarea`);
        area.focus();
        area.value = text;
        area.dispatchEvent(new InputEvent("input", { data: text, inputType: "insertText", bubbles: true }));
      },
      tab,
      token,
    );
    await browser.keys(["Enter"]);
    const read = await browser
      .waitUntil(() => paneText(session.tmux).includes(`agent read: ${token}`), { timeout: 5_000 })
      .then(() => true, () => false);
    if (read) return;
    if (attempt === 6) assert.fail(`the terminal never took the keyboard: ${JSON.stringify(await focusState())}`);
  }
}

/** Open the palette by its chord, type the query, and read back where it went. */
async function typeIntoPalette(session) {
  await browser.keys([...MOD, "k"]);
  await $(".palette-query").waitForExist({ timeout: 30_000 });
  await browser.keys(QUERY);
  // The pane hears a leaked keystroke a moment later than the window does.
  await browser.pause(1_500);
  await shot(`palette-typed-${session.id}`);
  const query = await $(".palette-query").getValue();
  const pane = paneText(session.tmux);
  return { query, pane, focus: await focused() };
}

async function ready(session) {
  await $(".sidebar").waitForExist({ timeout: 90_000 });
  await browser.waitUntil(async () => !(await $(".banner").isExisting()), {
    timeout: 90_000,
    timeoutMsg: "the sidecar never connected: the window still shows a banner",
  });
  await $(`.session-row[data-session="${session.id}"]`).waitForExist({ timeout: 30_000 });
  await backToSessionList();
}

/**
 * Bring the reducer back to its session list when another spec in this
 * world left it on the git view: a row chosen there is refused, so no tab
 * opens and no terminal takes the keyboard. Asked through the palette, the
 * way a person leaves the view; a greyed row means the list is already up.
 */
async function backToSessionList() {
  if (!(await $(".palette-query").isExisting())) {
    await browser.keys([...MOD, "k"]);
    await $(".palette-query").waitForExist({ timeout: 30_000 });
  }
  await setPaletteQuery("git_view.back");
  const row = await $('.palette-row[data-row="command:git_view.back"]');
  const drawn = await row.waitForExist({ timeout: 5_000 }).catch(() => false);
  if (drawn !== false && (await row.isEnabled())) {
    await browser.waitUntil(
      async () =>
        (await browser.execute(
          () => document.querySelector('.palette-row[aria-selected="true"]')?.getAttribute("data-row") ?? "",
        )) === "command:git_view.back",
      { timeout: 30_000 },
    );
    await browser.keys(["Enter"]);
  } else {
    await browser.keys(["Escape"]);
  }
  await browser.waitUntil(async () => !(await $(".palette-query").isExisting()), {
    timeout: 30_000,
    timeoutMsg: "the palette stayed open while leaving the git view",
  });
}

describe("the palette over a terminal", () => {
  it("takes every keystroke when the terminal already has focus", async () => {
    const session = seeded()[0];
    await ready(session);
    await focusTerminal(session);

    const typed = await typeIntoPalette(session);
    assert.equal(typed.query, QUERY, `the palette query holds ${JSON.stringify(typed.query)}; focus is on ${typed.focus}`);
    assert.ok(!typed.pane.includes(QUERY), `the pane read the query:\n${typed.pane}`);

    await browser.keys(["Escape"]);
    await browser.waitUntil(async () => !(await $(".palette-query").isExisting()), {
      timeout: 30_000,
      timeoutMsg: "the palette stayed open after Escape",
    });
    assert.ok(!paneText(session.tmux).includes("^["), "Escape reached the pane");
  });

  it("keeps the keystrokes when a tab is focused under it", async () => {
    // The host focuses a terminal on its own schedule: a tab-open answer
    // arrives after the click that asked for it, and a tab accelerator lands
    // whenever it is pressed. Neither may take the keyboard from an open
    // palette. The accelerator is the deterministic way to ask for that
    // focus while the palette is up; the tab-open answer is the race the
    // commits journey hit on a fast machine.
    const session = seeded()[0];
    await ready(session);
    await focusTerminal(session);

    await browser.keys([...MOD, "k"]);
    await $(".palette-query").waitForExist({ timeout: 30_000 });
    await browser.keys([...MOD, "1"]);
    await browser.pause(300);
    await browser.keys(QUERY);
    await browser.pause(1_500);
    await shot("palette-typed-after-tab-accelerator");
    const query = await $(".palette-query").getValue();
    const pane = paneText(session.tmux);
    const focus = await focused();
    assert.equal(query, QUERY, `the palette query holds ${JSON.stringify(query)}; focus is on ${focus}`);
    assert.ok(!pane.includes(QUERY), `the pane read the query:\n${pane}`);

    await browser.keys(["Escape"]);
    await browser.waitUntil(async () => !(await $(".palette-query").isExisting()), {
      timeout: 30_000,
      timeoutMsg: "the palette stayed open after Escape",
    });
    assert.ok(!paneText(session.tmux).includes("^["), "Escape reached the pane");
  });

  it("moves the keyboard from the answer banner's composer on a person's tab chord", async () => {
    // The host's own focus stands down for a text field (the unit tests on
    // terminalMayTakeFocus pin that), but a tab chord is the person moving
    // the keyboard: pressed with the cursor in the composer, the terminal
    // takes it, the draft stays where it was typed, and the next Enter is
    // the pane's.
    const session = seeded()[0];
    await ready(session);
    // Raised with no session id, matched to the row by its worktree: the
    // banner over the row's terminal is what this case needs, not the id.
    hook({
      event_id: `e2e-palette-ask-${Date.now()}`,
      ts: Date.now(),
      session_id: "",
      cwd: session.cwd,
      event_type: "PreToolUse",
      matcher: "AskUserQuestion",
      agent: "claude",
      payload: {
        tool_name: "AskUserQuestion",
        tool_input: { questions: [{ question: "Which environment?", options: [{ label: "staging" }, { label: "prod" }] }] },
      },
    });
    await focusTerminal(session);
    await $(".answer-banner .answer-composer input").waitForExist({ timeout: 60_000 });
    await click(".answer-banner .answer-composer input");
    await browser.keys(QUERY);
    assert.equal(await $(".answer-banner .answer-composer input").getValue(), QUERY, "the composer took the draft");

    // The chord for this session's own tab: another spec's tab may sit ahead
    // of it in the strip, and the strip and the mounted terminals share one
    // order.
    const index = await browser.execute(
      (key) => [...document.querySelectorAll(".terminal[data-tab]")].findIndex((el) => el.getAttribute("data-tab") === key),
      session.tmux,
    );
    assert.ok(index >= 0, `the session's terminal is mounted`);
    const before = linesRead(session);
    await browser.keys([...MOD, String(index + 1)]);
    await browser.pause(300);
    await browser.keys(["Enter"]);
    await browser.waitUntil(() => linesRead(session) > before, {
      timeout: 30_000,
      timeoutMsg: `the terminal never took the keyboard from the composer: ${JSON.stringify(await focusState())}`,
    });
    await shot("composer-draft-after-tab-chord");
    assert.equal(await $(".answer-banner .answer-composer input").getValue(), QUERY, "the draft stayed in the composer");
    assert.ok(!paneText(session.tmux).includes(QUERY), "the draft never reached the pane");
  });

  it("keeps the keystrokes when the chord lands while the tab is still opening", async () => {
    const session = seeded()[1];
    await ready(session);
    // The row is chosen and the chord follows at once: the host's tab-open
    // answer, and the focus it asks for, arrive after the palette is up.
    await click(`.session-row[data-session="${session.id}"]`);
    const typed = await typeIntoPalette(session);
    // The tab is open by now: what the pane and the palette hold is settled.
    await browser.waitUntil(async () => (await $$(".tab[data-state]")).length > 0, {
      timeout: 60_000,
      timeoutMsg: "the terminal tab never opened",
    });
    await browser.pause(1_000);
    const after = { query: await $(".palette-query").isExisting() ? await $(".palette-query").getValue() : "(closed)", pane: paneText(session.tmux), focus: await focused() };

    assert.equal(typed.query, QUERY, `the palette query holds ${JSON.stringify(typed.query)}; focus is on ${typed.focus}`);
    assert.ok(!after.pane.includes(QUERY), `the pane read the query:\n${after.pane}`);
    assert.ok(after.focus.includes("palette-query"), `focus left the palette for ${after.focus}`);

    await browser.keys(["Escape"]);
    await browser.waitUntil(async () => !(await $(".palette-query").isExisting()), {
      timeout: 30_000,
      timeoutMsg: "the palette stayed open after Escape",
    });
  });
});
