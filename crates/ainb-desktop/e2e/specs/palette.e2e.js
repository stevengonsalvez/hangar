// The palette over a terminal (#47): with a terminal tab focused, the chord
// opens the palette and every keystroke after it is the palette's, whether
// the tab had focus for a while or was still being opened when the chord
// landed. What the pane read is checked from the pane itself, so a leaked
// keystroke is caught where it does damage rather than where it was typed.

import assert from "node:assert/strict";
import { click } from "../support.js";
import { paneText, seeded } from "../world.js";

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

/** Whether the terminal's own textarea has focus. */
async function terminalFocused() {
  return browser.execute(() => document.activeElement?.classList.contains("xterm-helper-textarea") ?? false);
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
}

describe("the palette over a terminal", () => {
  it("takes every keystroke when the terminal already has focus", async () => {
    const session = seeded()[0];
    await ready(session);
    await click(`.session-row[data-session="${session.id}"]`);
    await browser.waitUntil(terminalFocused, {
      timeout: 60_000,
      timeoutMsg: "the terminal never took focus after the row was chosen",
    });

    const typed = await typeIntoPalette(session);
    assert.equal(typed.query, QUERY, `the palette query holds ${JSON.stringify(typed.query)}; focus is on ${typed.focus}`);
    assert.ok(!typed.pane.includes(`agent read: ${QUERY}`), `the pane read the query:\n${typed.pane}`);

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
    await click(`.session-row[data-session="${session.id}"]`);
    await browser.waitUntil(terminalFocused, {
      timeout: 60_000,
      timeoutMsg: "the terminal never took focus after the row was chosen",
    });

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
