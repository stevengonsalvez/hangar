// The palette over a terminal (#47): with a terminal tab focused, the chord
// opens the palette and every keystroke after it is the palette's, whether
// the tab had focus for a while or was still being opened when the chord
// landed. What the pane read is checked from the pane itself, so a leaked
// keystroke is caught where it does damage rather than where it was typed.

import assert from "node:assert/strict";
import { click, intentsSent, setPaletteQuery } from "../support.js";
import { paneText, raiseHook, seeded } from "../world.js";

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

/**
 * The AskUserQuestion call, as Claude hands it to its PreToolUse hook. Raised
 * under the session's own id (the one ainb minted for it), which is what
 * places the question on the row since #101; a line with no id places
 * nothing.
 */
const ASK = {
  event: "PreToolUse",
  matcher: "AskUserQuestion",
  payload: {
    tool_name: "AskUserQuestion",
    tool_input: { questions: [{ question: "Which environment?", options: [{ label: "staging" }, { label: "prod" }] }] },
  },
};

/**
 * The request id of the banner over the selected row once it shows a
 * question nobody has answered: the one just raised, not the one the case
 * before this answered, whose banner lingers as delivered.
 */
async function bannerUp() {
  let phase = "";
  const fresh = async () => {
    phase = await browser.execute(() => document.querySelector(".answer-banner[data-request] .answer-phase")?.dataset.phase ?? "");
    return phase === "none";
  };
  await browser.waitUntil(fresh, { timeout: 60_000 }).catch(() => assert.fail(`no unanswered question came up (last phase: ${phase || "no banner"})`));
  return $(".answer-banner[data-request]").getAttribute("data-request");
}

/**
 * Answer `request`'s question with its first option and wait for the
 * reducer to record the delivery, as the answer spec does, so no case leaves
 * a question open over the row the next case uses.
 */
async function answerBanner(request, session) {
  await click('.answer-banner .answer-option[data-option="0"]');
  let phase = "";
  const delivered = async () => {
    phase = await browser.execute(
      (id) => document.querySelector(`.answer-banner[data-request="${id}"] .answer-phase`)?.dataset.phase ?? "",
      request,
    );
    return phase === "delivered";
  };
  await browser.waitUntil(delivered, { timeout: 60_000 }).catch(() =>
    assert.fail(`the answer never read delivered (last phase: ${phase || "none"}; pane: ${paneText(session.tmux).trim().split("\n").slice(-3).join(" / ")})`),
  );
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

/** How many tab-strip answers the host has given, as the strip counts them. */
const hostAnswers = async () => Number(await $(".tabs").getAttribute("data-host-answers"));

describe("the palette over a terminal", () => {
  // Whatever a case left behind is put away here, so one failure cannot
  // spill into the next: a palette still open, and a question still open
  // over the row, whose composer would otherwise hold the last case's draft.
  afterEach(async () => {
    if (await $(".palette-query").isExisting()) {
      await browser.keys(["Escape"]);
      await $(".palette-query").waitForExist({ timeout: 30_000, reverse: true });
    }
    if (!(await $(".answer-banner[data-request]").isExisting())) return;
    const phase = await browser.execute(() => document.querySelector(".answer-banner[data-request] .answer-phase")?.dataset.phase ?? "");
    if (phase === "delivered" || phase === "already_answered") return;
    await answerBanner(await $(".answer-banner[data-request]").getAttribute("data-request"), seeded()[0]);
  });

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
    raiseHook(session, ASK);
    await focusTerminal(session);
    await bannerUp();
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
    await browser.keys([...MOD, String(index + 1)]);
    // The move itself: the keyboard is on this tab's own textarea, which the
    // shell focuses from a frame after the chord.
    const onTerminal = () =>
      browser.execute(
        (tab) => document.activeElement === document.querySelector(`${tab} .xterm-helper-textarea`),
        `.terminal[data-tab="${session.tmux}"]`,
      );
    let state = null;
    await browser
      .waitUntil(onTerminal, { timeout: 10_000 })
      .catch(async () => {
        state = await focusState();
        assert.fail(`the chord never moved the keyboard from the composer: ${JSON.stringify(state)}`);
      });
    // And the pane hears it: an Enter through the driver reaches the agent,
    // which echoes the (empty) line it read. The Linux driver drops a key
    // now and then, as focusTerminal's tries allow for, so this asks more
    // than once; the move is already proven above.
    let read = false;
    for (let attempt = 1; attempt <= 3 && !read; attempt += 1) {
      const before = linesRead(session);
      await browser.keys(["Enter"]);
      read = await browser
        .waitUntil(() => linesRead(session) > before, { timeout: 5_000 })
        .then(() => true, () => false);
    }
    assert.ok(read, `the pane never read the Enter after the chord: ${JSON.stringify(await focusState())}; pane tail: ${paneText(session.tmux).trim().split("\n").slice(-4).join(" / ")}`);
    await shot("composer-draft-after-tab-chord");
    assert.equal(await $(".answer-banner .answer-composer input").getValue(), QUERY, "the draft stayed in the composer");
    assert.ok(!paneText(session.tmux).includes(QUERY), "the draft never reached the pane");
  });

  it("keeps the keyboard in the composer when the host opens a tab on its own", async () => {
    // The other half of the rule the chord case pins: the host's own answer to
    // a tab open, landing while the cursor is in the composer with a draft,
    // leaves the keyboard where it is. Without the `byHost` flag on that
    // answer the terminal would take it, and the rest of the reply would go
    // to the agent's pane (#47).
    const session = seeded()[0];
    await ready(session);
    raiseHook(session, ASK);
    await focusTerminal(session);
    await bannerUp();
    await $(".answer-banner .answer-composer input").waitForExist({ timeout: 60_000 });
    await click(".answer-banner .answer-composer input");
    await browser.keys(QUERY);
    assert.equal(await $(".answer-banner .answer-composer input").getValue(), QUERY, "the composer took the draft");

    // The host's tab open, with nothing else touching the keyboard: the row's
    // own click handler, run by the page rather than by a pointer press that
    // would move focus off the composer before the host answered. The tab is
    // already open, so the host answers with focus on it, as for a fresh one.
    const answers = await hostAnswers();
    await browser.execute((id) => document.querySelector(`.session-row[data-session="${id}"]`).click(), session.id);
    await browser.waitUntil(async () => (await hostAnswers()) > answers, {
      timeout: 30_000,
      timeoutMsg: "the host never answered the row's click with its tabs",
    });
    // The frame the shell would focus the terminal from is the one after the
    // answer: two frames on, whatever it did is done.
    await browser.execute(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
    await browser.keys("x");
    await browser.pause(1_500);
    await shot("composer-draft-after-host-tab-open");
    const state = await focusState();
    assert.equal(
      await $(".answer-banner .answer-composer input").getValue(),
      `${QUERY}x`,
      `the composer lost the keyboard to ${state.active}`,
    );
    assert.ok(!paneText(session.tmux).includes(QUERY), `the draft reached the pane:\n${paneText(session.tmux)}`);
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
