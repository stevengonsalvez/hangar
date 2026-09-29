// The whole window, mounted over a fake host: Cmd+U (Ctrl+Shift+U off macOS)
// jumps to the next agent in the board's Needs you column and reveals it. On
// the board it focuses that card and selects its row, as a click on the card
// does; anywhere else it shows the agent's tab, opening it when none is open.
// Pressed again it moves down the column and wraps. From Settings it leaves
// the page first (`answer_home`), so the host's screen gate refuses nothing.
// An ACP agent, which has no session row, opens its transcript. With nothing
// waiting it sends nothing and toasts "Nobody needs you": Orca has no such
// chord, so there is no silence to match, and a dead chord reads as broken.

import "./window.ts";

import assert from "node:assert/strict";
import { test } from "node:test";

type Intent = { Command: [string, unknown] } | { Key: unknown } | { Text: string };
type Callback = (payload: unknown) => void;
type Sent = { id: string; session?: string; open?: boolean };

const HOST = "host-1";
/** A (api) and B (web) wait on a human; C (cli) is working. Only A has a tab. */
const TAB_A = { key: "tmux_api", target: { kind: "session", id: "u-1", tmux: "tmux_api" }, state: "attached" };
const TAB_B = { key: "tmux_web", target: { kind: "session", id: "u-2", tmux: "tmux_web" }, state: "attached" };
const A = "claude:p-1";
const B = "claude:p-2";

/** The fake host: the reducer's screen, its tab strip, what the window sent. */
const host = {
  screen: "session_list",
  version: 1,
  tabs: [TAB_A] as unknown[],
  sent: [] as Sent[],
  frames: undefined as undefined | { onmessage: Callback },
  events: new Map<string, Callback[]>(),
};

function frame(section: string, body: unknown) {
  host.version += 1;
  return { section, version: host.version, epoch: 1, host_id: HOST, daemon_read: null, body };
}

const shellBody = () => ({ current_screen: host.screen, previous_screen: null, notifications: [] });

function sessionsBody(selected: string | null) {
  const row = (id: string, name: string) => ({
    id,
    name,
    status: "Running",
    branch_name: `ainb/${name}`,
    workspace_path: "/repo",
    attention: [],
  });
  return {
    workspaces: [{ name: "repo", path: "/repo", shell_session: null, sessions: [row("u-1", "api"), row("u-2", "web"), row("u-3", "cli")] }],
    selected_session_id: selected,
    shell_selected: false,
  };
}

const fleetBody = {
  fleet_metadata: {
    "u-1": { provider_session_id: "p-1" },
    "u-2": { provider_session_id: "p-2" },
    "u-3": { provider_session_id: "p-3" },
  },
  fleet_snapshot: [],
  daemon_attention: { by_session_id: {}, all: {}, reachable: true, error: null, not_running: false },
  attention_elsewhere: 0,
};

/** The host's agent status: `waiting` lists the provider ids blocked on a
 * human; `acpWaiting` adds an ACP agent (no session row) blocked on one too. */
function statusBody(waiting: string[], acpWaiting = false) {
  const card = (id: string) => ({
    session_key: `claude:${id}`,
    provider: "claude",
    lifecycle: "RUNNING",
    transport_health: "HEALTHY",
    state: waiting.includes(id) ? "waiting" : "working",
    has_open_request: false,
    wait_kind: waiting.includes(id) ? "ask" : null,
    turn_complete: false,
    evidence_observed_at: 1,
    tier: "hook",
  });
  return {
    absent: null,
    head_revision: host.version,
    view: {
      host_id: HOST,
      read_revision: host.version,
      received_at_ms: 1,
      head_revision: host.version,
      health: { kind: "live" },
      cards: [
        ...["p-1", "p-2", "p-3"].map(card),
        ...(acpWaiting ? [{ ...card("acp-1"), session_key: "acp:acp-1", provider: "acp", state: "waiting" }] : []),
      ],
    },
  };
}

/** Put the reducer on `screen` and frame it, as the host does after a move. */
function show(screen: string): void {
  host.screen = screen;
  host.frames?.onmessage({ frames: [frame("shell", shellBody())] });
}

/** The host's tab strip changing: `focus` is the tab its answer shows. */
function tabsChanged(tabs: unknown[], focus: string | null = null): void {
  host.tabs = tabs;
  for (const handler of host.events.get("terminal_tabs") ?? []) {
    handler({ event: "terminal_tabs", id: 0, payload: { tabs, focus } });
  }
}

/** The host's answer to one intent: a row off the session list is refused,
 * as the real host's screen gate does; opening a row with no tab opens one. */
function dispatch(intent: Intent): unknown {
  if (!("Command" in intent)) return null;
  const [id, args] = intent.Command;
  const target = (args as { target?: { session?: string }; open?: boolean } | undefined)?.target;
  host.sent.push({ id, session: target?.session, open: (args as { open?: boolean } | undefined)?.open });
  if (id === "session_list.select_row") {
    if (host.screen !== "session_list") return { command: id, reason: "it is not in context on this screen" };
    host.frames?.onmessage({ frames: [frame("sessions", sessionsBody(target?.session ?? null))] });
    const open = (args as { open: boolean }).open;
    if (open && target?.session === "u-2" && !host.tabs.includes(TAB_B)) {
      setTimeout(() => tabsChanged([...host.tabs, TAB_B], TAB_B.key), 0);
    }
  }
  const pages: Record<string, string> = { "home.config": "config", "home.inbox": "inbox", "home.sessions": "session_list" };
  if (id in pages) show(pages[id]);
  return null;
}

const callbacks = new Map<number, Callback>();
let nextCallback = 1;
(window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {
  transformCallback: (callback: Callback) => {
    callbacks.set(nextCallback, callback);
    return nextCallback++;
  },
  unregisterCallback: () => undefined,
  invoke: async (command: string, args: Record<string, unknown> = {}) => {
    switch (command) {
      case "plugin:event|listen": {
        const handlers = host.events.get(args.event as string) ?? [];
        handlers.push(callbacks.get(args.handler as number)!);
        host.events.set(args.event as string, handlers);
        return args.handler;
      }
      case "sidecar_state":
        return { state: "running" };
      case "terminal_tabs":
        return { tabs: host.tabs, focus: null };
      case "subscribe":
        host.frames = args.frames as { onmessage: Callback };
        host.frames.onmessage({
          frames: [
            frame("shell", shellBody()),
            frame("sessions", sessionsBody(null)),
            frame("fleet", fleetBody),
            frame("agent_status", statusBody(["p-1", "p-2"])),
          ],
        });
        return HOST;
      case "dispatch":
        return dispatch(args.intent as Intent);
      case "answer_home":
        host.sent.push({ id: "answer_home" });
        if (host.screen !== "session_list") show("session_list");
        return null;
      default:
        return null;
    }
  },
};

// The window's own globals happy-dom does not hand Node by default.
for (const name of ["requestAnimationFrame", "cancelAnimationFrame", "ResizeObserver", "getComputedStyle", "localStorage"]) {
  const value = (window as unknown as Record<string, unknown>)[name];
  Object.defineProperty(globalThis, name, {
    value: typeof value === "function" && name !== "ResizeObserver" ? (value as () => unknown).bind(window) : value,
    configurable: true,
    writable: true,
  });
}
Object.defineProperty(window, "matchMedia", {
  value: () => ({ matches: true, addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {} }),
  configurable: true,
  writable: true,
});

/** Wait until `ready` holds, a few event-loop turns at a time. */
async function until(ready: () => boolean, what: string): Promise<void> {
  for (let turn = 0; turn < 200; turn += 1) {
    if (ready()) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  assert.fail(`timed out waiting for ${what}`);
}

/** Let anything a press sent come back, and any refusal toast. */
const drain = () => new Promise((resolve) => setTimeout(resolve, 100));

/** Cmd+U as this window reads it off macOS (the test runner is not a Mac). */
function pressAttention(): void {
  document.body.dispatchEvent(
    new window.KeyboardEvent("keydown", { code: "KeyU", key: "U", ctrlKey: true, shiftKey: true, bubbles: true, cancelable: true }),
  );
}

const boardShown = () => document.querySelector(".board") !== null;
const settingsShown = () => document.querySelector(".settings-page") !== null;
const needsCount = () => document.querySelector('.board-column[data-state="needs"] .board-count')?.textContent;
const focusedCard = () => (document.activeElement as HTMLElement | null)?.dataset?.card;
const shownTerminal = () => document.querySelector<HTMLElement>(".terminal[data-tab]:not([hidden])")?.dataset.tab;
const toasts = () => [...document.querySelectorAll(".toast")].map((toast) => toast.textContent ?? "");
const refusals = () => toasts().filter((text) => text.includes("is not run from the window"));

test("on the board, Cmd+U reveals the waiting cards in turn: A, then B, then A", async () => {
  const root = document.createElement("div");
  root.id = "root";
  document.body.appendChild(root);
  await import("../../src/main.tsx");
  await until(() => boardShown() && needsCount() === "2", "the board with two agents in Needs you");

  for (const [card, session] of [
    [A, "u-1"],
    [B, "u-2"],
    [A, "u-1"],
  ]) {
    host.sent = [];
    pressAttention();
    await until(() => focusedCard() === card, `${card}'s card to take focus`);
    await drain();
    assert.ok(boardShown(), "the board stays: the card is revealed where it is drawn");
    assert.deepEqual(
      host.sent.slice(0, 2),
      [{ id: "answer_home" }, { id: "session_list.select_row", session, open: false }],
      "home first, then the card's row selected without attaching it, as a click on the card does",
    );
  }
  assert.deepEqual(refusals(), []);
});

test("off the board, Cmd+U opens B's tab, which it has none of, then shows A's", async () => {
  // The last press revealed A: the next one down is B.
  document.querySelector<HTMLElement>(".tab[data-state] .tab-title")!.click();
  await until(() => shownTerminal() === TAB_A.key, "A's terminal, clicked");
  await drain();
  host.sent = [];
  pressAttention();
  await until(() => shownTerminal() === TAB_B.key, "B's tab opened and shown");
  assert.deepEqual(host.sent.slice(0, 2), [{ id: "answer_home" }, { id: "session_list.select_row", session: "u-2", open: true }]);

  host.sent = [];
  pressAttention();
  await until(() => shownTerminal() === TAB_A.key, "A's tab shown again");
  await drain();
  assert.deepEqual(host.sent, [{ id: "answer_home" }, { id: "session_list.select_row", session: "u-1", open: false }]);
  assert.deepEqual(refusals(), []);
});

test("with Settings open, Cmd+U leaves Settings first, and nothing is refused", async () => {
  // B's tab closes, so this round takes the open path from Settings as well
  // as the tab path.
  tabsChanged([TAB_A]);
  await until(() => document.querySelectorAll(".tab[data-state]").length === 1, "B's tab closed");
  for (const [tab, open] of [
    [TAB_B, true],
    [TAB_A, false],
  ] as const) {
    show("config");
    await until(settingsShown, "the settings page");
    host.sent = [];
    pressAttention();
    await until(() => !settingsShown() && shownTerminal() === tab.key, `Settings closed and ${tab.key} shown`);
    await drain();
    assert.deepEqual(host.sent[0], { id: "answer_home" }, "home before any row");
    assert.deepEqual(host.sent[1], { id: "session_list.select_row", session: tab.target.id, open });
    assert.deepEqual(refusals(), [], `no refused select_row: the window sent ${JSON.stringify(host.sent)}`);
  }
});

test("an ACP agent waiting on you, which has no session row, opens its transcript", async () => {
  host.frames?.onmessage({ frames: [frame("agent_status", statusBody([], true))] });
  await until(() => document.querySelector(".statusbar-needs")?.textContent === "1 need you", "the ACP agent in Needs you");
  assert.equal(document.querySelector(".acp-card"), null, "no transcript before the press");
  host.sent = [];
  pressAttention();
  await until(() => document.querySelector(".acp-card") !== null, "the ACP transcript card");
  assert.equal(document.querySelector(".transcript-tab .tab-title")?.textContent, "acp:acp-1");
  assert.equal(shownTerminal(), undefined, "the transcript holds the work area, not a terminal");
  await drain();
  assert.deepEqual(host.sent[0], { id: "answer_home" }, "home before the transcript is asked for");
  assert.deepEqual(refusals(), []);
  document.querySelector<HTMLElement>(".acp-close")!.click();
  await until(() => document.querySelector(".acp-card") === null, "the transcript closed");
});

test("with nothing waiting, Cmd+U sends nothing and says so once", async () => {
  host.frames?.onmessage({ frames: [frame("agent_status", statusBody([]))] });
  await until(() => document.querySelector(".statusbar-needs") === null, "the footer to say nobody needs you");
  const nobody = () => toasts().filter((text) => text === "Nobody needs you").length;
  const before = toasts().length;
  document.querySelector<HTMLElement>(".tab[data-state] .tab-title")!.click();
  await until(() => shownTerminal() !== undefined, "a terminal shown");
  await drain();
  const shown = shownTerminal();
  host.sent = [];
  pressAttention();
  await drain();
  assert.deepEqual(host.sent, [], "nothing sent");
  assert.equal(shownTerminal(), shown, "the same terminal still shown");
  assert.equal(toasts().length, before + 1, "exactly one toast");
  assert.equal(nobody(), 1, "and it says nobody needs you");

  // The board too: no card takes focus, and one more toast says the same.
  document.querySelector<HTMLElement>(".board-tab .tab-title")!.click();
  await until(() => boardShown() && needsCount() === "0", "the board with Needs you empty");
  host.sent = [];
  pressAttention();
  await drain();
  assert.deepEqual(host.sent, []);
  assert.equal(focusedCard(), undefined);
  assert.equal(toasts().length, before + 2);
  assert.equal(nobody(), 2);
});
