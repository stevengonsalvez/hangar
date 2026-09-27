// The empty work area mounted: after a tab closes, the selected session is
// named and its terminal is one click away; with nothing selected, the prompt.

import { settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent } from "solid-js";
import { render } from "solid-js/web";
import type { EmptyPane as EmptyPaneView } from "../../src/pane_empty.ts";
import { EmptyPane } from "../../src/pane_empty.tsx";

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  document.body.innerHTML = "";
});

async function mount(view: EmptyPaneView) {
  const opened: string[] = [];
  const container = document.createElement("section");
  container.className = "workarea";
  document.body.appendChild(container);
  cleanup = render(() => createComponent(EmptyPane, { view, onOpen: (id) => opened.push(id) }), container);
  await settle();
  return { container, opened };
}

test("the selected session is named, and Open terminal opens that session", async () => {
  const { container, opened } = await mount({ kind: "selected", sessionId: "s-2", name: "docs", branch: "ainb/docs" });
  assert.equal(container.textContent?.includes("Choose a session"), false, "no 'choose' beside a chosen row");
  assert.equal(container.querySelector(".empty-name")?.textContent, "docs");
  assert.equal(container.querySelector(".empty-branch")?.textContent, "ainb/docs");
  container.querySelector<HTMLButtonElement>(".empty-open")!.click();
  assert.deepEqual(opened, ["s-2"]);
});

test("with nothing selected, it asks the person to choose", async () => {
  const { container } = await mount({ kind: "choose" });
  assert.match(container.textContent ?? "", /Choose a session to open its terminal/);
  assert.equal(container.querySelector(".empty-open"), null);
});
