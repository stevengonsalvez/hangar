// The window's own chrome at its default size (1280x800, `tauri.conf.json`):
// the defects a driven run on a real machine found, which no unit test can
// see because only a real webview lays the page out.
//
// - every board column fits inside the board, none clipped past its edge;
// - a project's chevron points down while open and right while collapsed;
// - the active terminal tab shows its close control, and the control closes it.

import assert from "node:assert/strict";
import { click } from "../support.js";
import { seeded } from "../world.js";

/** The sidebar is drawn and the sidecar is connected. */
async function connected() {
  await $(".sidebar").waitForExist({ timeout: 90_000 });
  await browser.waitUntil(async () => !(await $(".banner").isExisting()), {
    timeout: 90_000,
    timeoutMsg: "the sidecar never connected: the window still shows a banner",
  });
}

/**
 * Which way the first project's chevron points, from the rotation its
 * `::before` is drawn with: the two borders make a corner pointing down-right,
 * so a positive turn points it down and a negative one points it right.
 */
async function chevron() {
  return browser.execute(() => {
    const summary = document.querySelector(".workspace .workspace-name");
    const transform = getComputedStyle(summary, "::before").transform;
    const match = /matrix\(([^,]+),\s*([^,]+)/.exec(transform);
    if (!match) return `unparsed ${transform}`;
    return Number(match[2]) > 0 ? "down" : "right";
  });
}

describe("the window at its default size", () => {
  it("fits every board column inside the board", async () => {
    await connected();
    await click(".board-tab .tab-title");
    await $(".board-columns").waitForExist({ timeout: 30_000 });
    const fit = await browser.execute(() => {
      const area = document.querySelector(".board-columns");
      const edge = area.getBoundingClientRect().right;
      return {
        width: window.innerWidth,
        overflow: area.scrollWidth - area.clientWidth,
        columns: [...area.querySelectorAll(".board-column")].map((column) => ({
          state: column.dataset.state,
          past: Math.round(column.getBoundingClientRect().right - edge),
        })),
      };
    });
    assert.equal(fit.columns.length, 4, JSON.stringify(fit));
    assert.ok(fit.overflow <= 1, `the columns overflow the board by ${fit.overflow}px: ${JSON.stringify(fit)}`);
    for (const column of fit.columns) {
      assert.ok(column.past <= 1, `the ${column.state} column runs ${column.past}px past the board: ${JSON.stringify(fit)}`);
    }
  });

  it("points a project's chevron down while open and right while collapsed", async () => {
    await connected();
    await $(".workspace[open] .workspace-name").waitForExist({ timeout: 30_000 });
    await browser.waitUntil(async () => (await chevron()) === "down", {
      timeout: 5_000,
      timeoutMsg: "an open project's chevron does not point down",
    });
    await click(".workspace .workspace-name");
    await browser.waitUntil(async () => (await chevron()) === "right", {
      timeout: 5_000,
      timeoutMsg: "a collapsed project's chevron does not point right",
    });
    // Open again, so the rows are there for whatever runs next.
    await click(".workspace .workspace-name");
    await $(".workspace[open]").waitForExist({ timeout: 5_000 });
  });

  it("shows the active terminal tab's close control, and it closes the tab", async () => {
    await connected();
    const session = seeded()[0];
    await click(`.session-row[data-session="${session.id}"]`);
    await $(".tab.active .tab-close").waitForExist({ timeout: 30_000 });
    await browser.waitUntil(
      async () =>
        (await browser.execute(() => getComputedStyle(document.querySelector(".tab.active .tab-close")).opacity)) === "1",
      { timeout: 5_000, timeoutMsg: "the active tab's close control is not visible" },
    );
    const before = (await $$(".tab[data-state]")).length;
    await click(".tab.active .tab-close");
    await browser.waitUntil(async () => (await $$(".tab[data-state]")).length === before - 1, {
      timeout: 15_000,
      timeoutMsg: "the close control did not close the tab",
    });
    // Closing a tab detaches the window from the session; the session itself
    // runs on, so its row stays in the sidebar.
    await browser.pause(1_000);
    assert.ok(
      await $(`.session-row[data-session="${session.id}"]`).isExisting(),
      "closing the tab removed the session's row: it should only have detached",
    );
    // With no tab left, the work area names the session its row still has
    // selected and offers the way back in, never "Choose a session" beside a
    // chosen row (#193). The last tab is closed first when more than one was
    // open, so the empty pane is what shows.
    while ((await $$(".tab[data-state]")).length > 0) {
      const count = (await $$(".tab[data-state]")).length;
      await click(".tab[data-state] .tab-close");
      await browser.waitUntil(async () => (await $$(".tab[data-state]")).length < count, { timeout: 15_000 });
    }
    // Rows redraw as frames land: read the selected one's id once it is drawn.
    await $(".session-row.selected").waitForExist({
      timeout: 15_000,
      timeoutMsg: "no sidebar row is selected after the tabs closed",
    });
    const selectedId = await $(".session-row.selected").getAttribute("data-session");
    await $(`.empty-selected[data-session="${selectedId}"]`).waitForExist({
      timeout: 15_000,
      timeoutMsg: "the empty pane does not name the selected session",
    });
    await click(".empty-selected .empty-open");
    await browser.waitUntil(async () => (await $$(".tab[data-state]")).length === 1, {
      timeout: 30_000,
      timeoutMsg: "Open terminal did not open the selected session's tab",
    });
  });
});
