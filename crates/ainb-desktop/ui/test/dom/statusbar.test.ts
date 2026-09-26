// The status bar mounted: the host id, the daemon dot, and the counts, drawn
// from exactly the props `main.tsx` computes from `ROOT_SELECTORS` (that sum
// is `main.tsx`'s own concern; this only checks the bar draws what it is given).

import { settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent } from "solid-js";
import { render } from "solid-js/web";
import { Statusbar } from "../../src/statusbar.tsx";

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  document.body.innerHTML = "";
});

async function open(over: Partial<Parameters<typeof Statusbar>[0]> = {}) {
  const container = document.createElement("div");
  document.body.appendChild(container);
  cleanup = render(
    () =>
      createComponent(Statusbar, {
        host: "host-1",
        sidecar: { state: "connected", spawned: true, daemon_version: "1.0", protocol: { min: 1, max: 1 } },
        needsYou: 0,
        idle: 0,
        ...over,
      }),
    container,
  );
  await settle();
}

test("the host id and one need-you count draw, ASK/APPROVE/WAIT/ERR already summed", async () => {
  await open({ needsYou: 3, idle: 2 });
  const text = document.querySelector("footer.statusbar")?.textContent ?? "";
  assert.match(text, /host-1/);
  assert.match(text, /3 need you/);
  assert.match(text, /2 idle/);
});

test("nothing needing a human draws no amber item, only the idle count", async () => {
  await open({ needsYou: 0, idle: 5 });
  assert.equal(document.querySelector(".statusbar-needs"), null);
  assert.match(document.querySelector(".statusbar-idle")?.textContent ?? "", /5 idle/);
});

test("the daemon dot names the sidecar's own state", async () => {
  await open({ sidecar: { state: "degraded", error: "no daemon", has_log: false } });
  const dot = document.querySelector(".statusbar-daemon-dot");
  assert.equal(dot?.classList.contains("error"), true);
  assert.equal(dot?.getAttribute("title"), "degraded");
  // A live region with no text announces nothing: the state is spelled out.
  assert.match(dot?.textContent ?? "", /Daemon degraded/);
});
