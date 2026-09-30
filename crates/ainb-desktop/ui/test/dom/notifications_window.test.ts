// The whole window, mounted over a fake host: what the page tells the host
// for OS notifications (`notify.rs` on the Rust side). At start it sends the
// STORED toggle; Settings > Notifications sends every change; whenever what
// the work area shows changes it sends that session's card key, or null for
// the board, Settings and the Inbox; and a click on a notification, which the
// host relays as `notify:open`, selects that session's row.

import "./window.ts";

import assert from "node:assert/strict";
import { test } from "node:test";

type Intent = { Command: [string, unknown] } | { Key: unknown } | { Text: string };
type Callback = (payload: unknown) => void;
type Sent = { id: string; session?: string; open?: boolean };

const HOST = "host-1";
/** A (api, u-1) has a tab; B (web, u-2) does not. */
const TAB_A = { key: "tmux_api", target: { kind: "session", id: "u-1", tmux: "tmux_api" }, state: "attached" };
const A = "claude:p-1";
const B = "claude:p-2";

// Stored before the window loads: the page must tell the host this, not its default.
window.localStorage.setItem("ainb.notifications", "off");

const host = {
  screen: "session_list",
  version: 1,
  tabs: [TAB_A] as unknown[],
  sent: [] as Sent[],
  /** Every `notify_focus` and `notifications_set` call, in order. */
  told: [] as { command: string; args: unknown }[],
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
    workspaces: [{ name: "repo", path: "/repo", shell_session: null, sessions: [row("u-1", "api"), row("u-2", "web")] }],
    selected_session_id: selected,
    shell_selected: false,
  };
}

const fleetBody = {
  fleet_metadata: { "u-1": { provider_session_id: "p-1" }, "u-2": { provider_session_id: "p-2" } },
  fleet_snapshot: [],
  daemon_attention: { by_session_id: {}, all: {}, reachable: true, error: null, not_running: false },
  attention_elsewhere: 0,
};

function statusBody() {
  const card = (id: string) => ({
    session_key: `claude:${id}`,
    provider: "claude",
    lifecycle: "RUNNING",
    transport_health: "HEALTHY",
    state: "working",
    has_open_request: false,
    wait_kind: null,
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
      cards: ["p-1", "p-2"].map(card),
    },
  };
}

function show(screen: string): void {
  host.screen = screen;
  host.frames?.onmessage({ frames: [frame("shell", shellBody())] });
}

function dispatch(intent: Intent): unknown {
  if (!("Command" in intent)) return null;
  const [id, args] = intent.Command;
  const target = (args as { target?: { session?: string } } | undefined)?.target;
  host.sent.push({ id, session: target?.session, open: (args as { open?: boolean } | undefined)?.open });
  if (id === "session_list.select_row") {
    if (host.screen !== "session_list") return { command: id, reason: "it is not in context on this screen" };
    host.frames?.onmessage({ frames: [frame("sessions", sessionsBody(target?.session ?? null))] });
  }
  const pages: Record<string, string> = { "home.config": "config", "home.inbox": "inbox", "home.sessions": "session_list" };
  if (id in pages) show(pages[id]);
  return null;
}

/** The host relaying a click on a session's OS notification. */
function notificationClicked(sessionKey: string): void {
  for (const handler of host.events.get("notify:open") ?? []) {
    handler({ event: "notify:open", id: 0, payload: { session_key: sessionKey } });
  }
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
      case "notify_focus":
      case "notifications_set":
        host.told.push({ command, args });
        return null;
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
            frame("agent_status", statusBody()),
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

async function until(ready: () => boolean, what: string): Promise<void> {
  for (let turn = 0; turn < 200; turn += 1) {
    if (ready()) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  assert.fail(`timed out waiting for ${what}: told ${JSON.stringify(host.told)}`);
}

const drain = () => new Promise((resolve) => setTimeout(resolve, 100));
const settingsShown = () => document.querySelector(".settings-page") !== null;
const shownTerminal = () => document.querySelector<HTMLElement>(".terminal[data-tab]:not([hidden])")?.dataset.tab;
const toggles = () => host.told.filter((call) => call.command === "notifications_set").map((call) => call.args);
const focusTold = () => host.told.filter((call) => call.command === "notify_focus").map((call) => call.args);
const lastFocus = () => focusTold().at(-1);

test("at start the page tells the host the STORED toggle, and the board shows no session", async () => {
  const root = document.createElement("div");
  root.id = "root";
  document.body.appendChild(root);
  await import("../../src/main.tsx");
  await until(() => document.querySelector(".board") !== null, "the board");
  await drain();
  assert.deepEqual(toggles()[0], { enabled: false }, "the stored `off`, not the default `on`");
  assert.deepEqual(lastFocus(), { session: null }, "the board shows no one session");
});

test("the shown terminal's card key reaches the host, and Settings and the Inbox send null", async () => {
  document.querySelector<HTMLElement>(".tab[data-state] .tab-title")!.click();
  await until(() => shownTerminal() === TAB_A.key, "A's terminal");
  await until(() => lastFocus() !== undefined && (lastFocus() as { session: unknown }).session === A, "A's card key told");
  assert.deepEqual(lastFocus(), { session: A }, "the card key, not the tab key or the row id");

  show("config");
  await until(settingsShown, "the settings page");
  await drain();
  assert.deepEqual(lastFocus(), { session: null }, "Settings covers the terminal");

  show("session_list");
  await until(() => !settingsShown() && shownTerminal() === TAB_A.key, "back on A's terminal");
  await drain();
  assert.deepEqual(lastFocus(), { session: A });

  show("inbox");
  await until(() => document.querySelector('section.inbox[aria-label="Inbox"]') !== null, "the inbox page");
  await drain();
  assert.deepEqual(lastFocus(), { session: null }, "the Inbox covers the terminal");
  show("session_list");
  await drain();
});

test("Settings > Notifications sends each change to the host", async () => {
  show("config");
  await until(settingsShown, "the settings page");
  const toggle = document.querySelector<HTMLInputElement>("input[data-notifications-toggle]");
  assert.ok(toggle, "the Notifications toggle");
  assert.equal(toggle.checked, false, "drawn as stored");
  const before = toggles().length;
  toggle.click();
  await drain();
  assert.deepEqual(toggles().slice(before), [{ enabled: true }]);
  show("session_list");
  await drain();
});

test("a click on a session's notification selects that session's row", async () => {
  host.sent = [];
  notificationClicked(B);
  await until(() => host.sent.some((sent) => sent.id === "session_list.select_row"), "B's row selected");
  await drain();
  assert.deepEqual(host.sent, [
    { id: "answer_home" },
    { id: "session_list.select_row", session: "u-2", open: false },
  ]);

  host.sent = [];
  notificationClicked("claude:gone");
  await drain();
  assert.deepEqual(host.sent, [], "a session the window does not hold selects nothing");
});
