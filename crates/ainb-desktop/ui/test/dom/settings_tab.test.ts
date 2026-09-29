// The whole window, mounted over a fake host: with Settings or the Inbox open,
// a click on a terminal tab leaves the page and shows that tab, as Orca's
// activation does (it switches any page back to the terminal view before it
// activates the worktree). The fake host keeps the reducer's screen, refuses a
// session-list row off the session list as the real host's screen gate does,
// and walks home on `answer_home` as the real one does from any page
// (`tests/shell.rs`, `the_banners_sequence_lands_from_any_page`). A click that
// sends the row first shows the refusal toast.

import "./window.ts";

import assert from "node:assert/strict";
import { test } from "node:test";

type Intent = { Command: [string, unknown] } | { Key: unknown } | { Text: string };
type Callback = (payload: unknown) => void;

const HOST = "host-1";
const TAB = { key: "tmux_api", target: { kind: "session", id: "u-1", tmux: "tmux_api" }, state: "attached" };

/** The fake host: the reducer's screen, what the window sent, and its frames. */
const host = {
  screen: "config",
  version: 1,
  sent: [] as string[],
  frames: undefined as undefined | { onmessage: Callback },
  events: new Map<string, Callback[]>(),
};

function shellFrame() {
  host.version += 1;
  return {
    frames: [
      {
        section: "shell",
        version: host.version,
        epoch: 1,
        host_id: HOST,
        daemon_read: null,
        body: { current_screen: host.screen, previous_screen: null, notifications: [] },
      },
    ],
  };
}

/** The host's answer to one intent: a row off its screen is refused. */
function dispatch(intent: Intent): unknown {
  if (!("Command" in intent)) return null;
  const [id] = intent.Command;
  host.sent.push(id);
  if (id === "session_list.select_row" && host.screen !== "session_list") {
    return { command: id, reason: "it is not in context on this screen" };
  }
  if (id === "session_list.select_row") {
    const row = (intent.Command[1] as { target: { session?: string } }).target;
    if (row.session !== undefined) host.frames?.onmessage(sessionsFrame(row.session));
  }
  const pages: Record<string, string> = { "home.config": "config", "home.inbox": "inbox", "home.sessions": "session_list" };
  if (id in pages) show(pages[id]);
  return null;
}

/** The second tab: a session blocked on a question, for the close tests. */
const NEXT = { key: "tmux_web", target: { kind: "session", id: "u-2", tmux: "tmux_web" }, state: "attached" };

/** The session list with `selected` selected: `u-2` (NEXT's session) is
 * blocked on a question, so its banner draws once it is selected and shown. */
function sessionsFrame(selected: string) {
  host.version += 1;
  const row = (id: string, name: string, attention: unknown[]) => ({
    id,
    name,
    status: "Running",
    branch_name: `ainb/${name}`,
    workspace_path: "/repo",
    attention,
  });
  const ask = { kind: "Ask", request: "ASK:2", route: "Daemon", detail: "pick one", options: [] };
  return {
    frames: [
      {
        section: "sessions",
        version: host.version,
        epoch: 1,
        host_id: HOST,
        daemon_read: null,
        body: {
          workspaces: [
            { name: "repo", path: "/repo", shell_session: null, sessions: [row("u-1", "api", []), row("u-2", "web", [ask])] },
          ],
          selected_session_id: selected,
          shell_selected: false,
        },
      },
    ],
  };
}

/** The host's tab strip changing on its own schedule: a tab opened or closed. */
function tabsChanged(tabs: unknown[]): void {
  for (const handler of host.events.get("terminal_tabs") ?? []) {
    handler({ event: "terminal_tabs", id: 0, payload: { tabs, focus: null } });
  }
}

/** Put the reducer on `screen` and frame it, as the host does after a move. */
function show(screen: string): void {
  host.screen = screen;
  host.frames?.onmessage(shellFrame());
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
        return { tabs: [TAB], focus: null };
      case "subscribe":
        host.frames = args.frames as { onmessage: Callback };
        host.frames.onmessage(shellFrame());
        return HOST;
      case "dispatch":
        return dispatch(args.intent as Intent);
      case "answer_home":
        host.sent.push("answer_home");
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

const settingsShown = () => document.querySelector(".settings-page") !== null;
const inboxShown = () => document.querySelector(".inbox") !== null;
const toasts = () => [...document.querySelectorAll(".toast")].map((toast) => toast.textContent ?? "");
const terminal = () => document.querySelector<HTMLElement>(`.terminal[data-tab="${TAB.key}"]`);

/** Click the terminal tab over `page` and check it leaves for the tab. */
async function clickTabOver(page: string, shown: () => boolean): Promise<void> {
  await until(shown, `the ${page} page, the reducer being on it`);
  assert.equal(terminal()?.hidden, true, `the page holds the work area, not the tab`);
  host.sent = [];
  const title = document.querySelector<HTMLElement>(".tab[data-state] .tab-title");
  assert.ok(title, `the tab strip lists the tab under ${page}`);
  title.click();

  await until(() => !shown(), `${page} to close`);
  await until(() => terminal()?.hidden === false, "the tab's terminal to show");
  // Let any refusal the click drew come back and toast.
  await new Promise((resolve) => setTimeout(resolve, 100));
  assert.deepEqual(
    toasts().filter((text) => text.includes("is not run from the window")),
    [],
    `no refused intent: the window sent ${host.sent.join(", ")}`,
  );
  assert.deepEqual(host.sent, ["answer_home", "session_list.select_row"], "home first, then the tab's row");
}

test("a tab clicked from Settings closes Settings and shows the tab, with no refusal", async () => {
  const root = document.createElement("div");
  root.id = "root";
  document.body.appendChild(root);
  await import("../../src/main.tsx");
  await clickTabOver("Settings", settingsShown);
});

test("a tab clicked from the Inbox closes the Inbox and shows the tab, with no refusal", async () => {
  // The person opens the Inbox over the terminal the last test showed.
  show("inbox");
  await clickTabOver("Inbox", inboxShown);
});

test("closing the shown tab moves the sidebar to the next tab, and its question shows", async () => {
  // The Inbox test left the reducer home with TAB's terminal shown.
  tabsChanged([TAB, NEXT]);
  await until(() => document.querySelectorAll(".tab[data-state]").length === 2, "the second tab in the strip");
  host.sent = [];
  tabsChanged([NEXT]);

  await until(() => host.sent.includes("session_list.select_row"), "the next tab's row to be selected");
  await until(
    () => document.querySelector(".answer-banner")?.getAttribute("data-request") === "ASK:2",
    "the next tab's own question over its terminal",
  );
  assert.equal(document.querySelector<HTMLElement>(`.terminal[data-tab="${NEXT.key}"]`)?.hidden, false);
  assert.deepEqual(toasts().filter((text) => text.includes("is not run from the window")), []);
});

test("closing the shown tab under Settings or the Inbox selects no row and draws no refusal", async () => {
  for (const [page, screen, shown] of [
    ["Settings", "config", settingsShown],
    ["Inbox", "inbox", inboxShown],
  ] as const) {
    // A second tab to fall back to, then the page over the shown one.
    const shownKey = document.querySelector<HTMLElement>(".terminal:not([hidden])")?.dataset.tab;
    const other = shownKey === TAB.key ? NEXT : TAB;
    tabsChanged(shownKey === TAB.key ? [TAB, NEXT] : [NEXT, TAB]);
    await until(() => document.querySelectorAll(".tab[data-state]").length === 2, `the second tab under ${page}`);
    show(screen);
    await until(shown, `the ${page} page`);
    host.sent = [];
    tabsChanged([other]);
    await until(() => document.querySelectorAll(".tab[data-state]").length === 1, `the shown tab to close under ${page}`);
    // Let anything the close sent come back and toast.
    await new Promise((resolve) => setTimeout(resolve, 150));
    assert.deepEqual(host.sent, [], `no row sent under ${page}`);
    assert.deepEqual(toasts().filter((text) => text.includes("is not run from the window")), [], `no refusal under ${page}`);
    assert.ok(shown(), `${page} stays open`);
    // Home again, as a person would click a tab, for the next page's round.
    show("session_list");
    await until(() => !shown(), `${page} to close`);
  }
});
