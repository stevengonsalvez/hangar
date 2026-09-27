// A terminal tab mounted: its session's status as a glyph before the title,
// on `data-status` for the tab's tint, and in words for a screen reader.

import { settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent } from "solid-js";
import { render } from "solid-js/web";
import type { UiStatus } from "../../src/status.ts";
import { TerminalTab } from "../../src/terminal_tab.tsx";

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  document.body.innerHTML = "";
});

async function mount(status: UiStatus | null) {
  const container = document.createElement("div");
  document.body.appendChild(container);
  cleanup = render(
    () =>
      createComponent(TerminalTab, {
        tab: { key: "tmux_app", target: { kind: "session", id: "s-1", tmux: "tmux_app" }, state: "attached" } as never,
        title: "app",
        active: false,
        status,
        onChoose() {},
        onClose() {},
      }),
    container,
  );
  await settle();
  return container;
}

test("a needs-you tab carries the status, a shape glyph and the words", async () => {
  const container = await mount({ kind: "needs", need: "approve" });
  const tab = container.querySelector(".tab");
  assert.equal(tab?.getAttribute("data-status"), "needs-approve");
  const glyph = tab?.querySelector(".status-glyph");
  assert.equal(glyph?.getAttribute("data-status"), "needs-approve");
  assert.equal(glyph?.getAttribute("aria-hidden"), "true", "the shape is decorative");
  assert.match(tab?.querySelector(".tab-title")?.textContent ?? "", /Needs you · approve/, "the words are in the tab's name");
});

test("an exited tab says so, and a bare tmux tab draws no glyph", async () => {
  let container = await mount({ kind: "exited" });
  assert.equal(container.querySelector(".tab")?.getAttribute("data-status"), "exited");
  assert.match(container.querySelector(".tab-title")?.textContent ?? "", /Exited/);

  cleanup?.();
  document.body.innerHTML = "";
  container = await mount(null);
  assert.equal(container.querySelector(".tab")?.hasAttribute("data-status"), false);
  assert.equal(container.querySelector(".status-glyph"), null);
});
