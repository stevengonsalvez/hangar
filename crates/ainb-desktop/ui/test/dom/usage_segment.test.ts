// The status bar's usage segment mounted beside the stats tab, both fed from
// the window's one frame store the way `main.tsx` feeds them: one `usage`
// section, one accessor. A frame moves both; the segment asks the host for
// nothing of its own; a click hands the window its "open Stats".

import { hostCalls, settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent, createRoot } from "solid-js";
import { render } from "solid-js/web";
import type { Frame_Serialize, UsageBucketFrame, UsageView } from "../../../../ainb-app/bindings/AppState";
import { ROOT_SELECTORS } from "../../src/selectors.ts";
import { Stats } from "../../src/stats.tsx";
import { createFrameStore, type FrameStore } from "../../src/store.ts";
import { shellUsage, SUBSCRIBED } from "../../src/subscription.ts";
import { UsageSegment } from "../../src/usage_segment.tsx";

const HOST = "local";

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  document.body.innerHTML = "";
});

const bucket = (input: number, cost: number | null): UsageBucketFrame => ({
  input_tokens: input,
  cache_creation_tokens: 0,
  cache_read_tokens: 0,
  output_tokens: 0,
  reasoning_tokens: 0,
  call_count: 1,
  session_count: 1,
  project_count: 1,
  cost_usd: cost,
});

function ready(providers: [string, number][]): UsageView {
  return {
    absent: null,
    failure: null,
    summary: {
      state: "ready",
      totals: bucket(100, 1),
      daily: [],
      daily_cut: 0,
      providers: providers.map(([name, cost]) => ({ name, bucket: bucket(100, cost) })),
      providers_cut: 0,
      models: [],
      models_cut: 0,
      projects: [],
      projects_cut: 0,
      detail: null,
    },
  };
}

let version = 0;
function send(store: FrameStore, body: UsageView) {
  version += 1;
  const frame: Frame_Serialize = { section: "usage", version, epoch: 1, host_id: HOST, body };
  store.applyDrain(HOST, [{ frames: [frame] }]);
}

/** The window's store, the stats tab and the segment, wired as `main.tsx` wires them. */
async function mount(onOpen: () => void = () => undefined): Promise<FrameStore> {
  const { store, dispose } = createRoot((dispose) => ({ store: createFrameStore(SUBSCRIBED), dispose }));
  const usage = () => shellUsage(store, HOST);
  const usageStale = () => ROOT_SELECTORS.usageStale(store, HOST);
  const container = document.createElement("div");
  document.body.appendChild(container);
  const unrender = render(
    () => [
      createComponent(Stats, {
        get usage() {
          return usage();
        },
        get stale() {
          return usageStale();
        },
      }),
      createComponent(UsageSegment, {
        get usage() {
          return usage();
        },
        get stale() {
          return usageStale();
        },
        onOpen,
      }),
    ],
    container,
  );
  cleanup = () => {
    unrender();
    dispose();
  };
  await settle();
  return store;
}

const segment = () => document.querySelector<HTMLButtonElement>("button.statusbar-usage");
const chips = () =>
  [...document.querySelectorAll(".statusbar-usage-chip")].map((chip) => chip.textContent?.replace(/\s+/g, " ").trim());

test("before section 21 arrives the segment is loading, and says what it waits on", async () => {
  await mount();
  assert.equal(segment()?.dataset.state, "loading");
  assert.equal(segment()?.getAttribute("title"), "Reading usage from the daemon");
  assert.deepEqual(chips(), []);
});

test("the segment draws from the frame the stats tab draws, and moves with the next one", async () => {
  const store = await mount();
  send(store, ready([["claude", 12.5]]));
  await settle();
  assert.equal(segment()?.dataset.state, "ready");
  assert.deepEqual(chips(), ["claude $12.50"]);
  assert.match(document.querySelector(".stats")?.textContent ?? "", /\$12\.50/, "the stats tab drew the same frame");

  send(store, ready([["claude", 20], ["codex", 3]]));
  await settle();
  assert.deepEqual(chips(), ["claude $20.00", "codex $3.00"]);
  assert.match(document.querySelector(".stats")?.textContent ?? "", /\$20\.00/);
});

test("no summary to have draws the unavailable dash, the host's reason as its tooltip", async () => {
  const store = await mount();
  send(store, { absent: "the daemon does not serve fleet.usage.read", failure: null, summary: null });
  await settle();
  assert.equal(segment()?.dataset.state, "unavailable");
  assert.match(segment()?.textContent ?? "", /--/);
  assert.equal(segment()?.getAttribute("title"), "the daemon does not serve fleet.usage.read");
});

test("a failed refresh keeps the held numbers and marks them stale", async () => {
  const store = await mount();
  send(store, ready([["claude", 12.5]]));
  send(store, { ...ready([["claude", 12.5]]), failure: "socket closed" });
  await settle();
  assert.deepEqual(chips(), ["claude $12.50"]);
  assert.ok(document.querySelector(".statusbar-usage-stale"), "the stale mark is drawn");
  assert.match(segment()?.getAttribute("title") ?? "", /The last read failed: socket closed/);
});

test("a click opens Stats: the segment hands the window its one callback", async () => {
  let opened = 0;
  const store = await mount(() => (opened += 1));
  send(store, ready([["claude", 1]]));
  await settle();
  segment()?.click();
  assert.equal(opened, 1);
});

test("the segment makes no host call of its own: it rides the window's subscription", async () => {
  hostCalls.clear();
  const store = await mount();
  send(store, ready([["claude", 1]]));
  await settle();
  segment()?.click();
  await settle();
  assert.deepEqual([...hostCalls.keys()], [], "no subscribe, no poll, no read");
});
