// A fake host for tests that mount the whole window (`main.tsx`): the frames
// it subscribes to, the tab strip, and the intents it sends, answered the way
// the real host answers them. A row off the session list is refused, as the
// host's screen gate does; opening a row with no tab opens one and shows it.
//
// Import after `./window.ts` and before `main.tsx`. The window reads macOS
// from `navigator.userAgent` once, at import: call `pretendMac()` first to
// mount it as macOS does.

import assert from "node:assert/strict";

type Intent = { Command: [string, unknown] } | { Key: unknown } | { Text: string };
type Callback = (payload: unknown) => void;
export type Sent = { id: string; session?: string; open?: boolean };
type TabShape = { key: string; target: { kind: "session"; id: string; tmux: string }; state: "attached" };

const HOST = "host-1";

/** A session's tab, keyed as the host keys it. */
export const tabOf = (id: string): TabShape => ({
  key: `tmux_${id}`,
  target: { kind: "session", id, tmux: `tmux_${id}` },
  state: "attached",
});

/**
 * Three worktrees in two projects, in sidebar order: `u-1` and `u-2` in
 * project a, `u-3` in project b. `u-1` and `u-2` have tabs open.
 */
export const host = {
  screen: "session_list",
  version: 1,
  selected: null as string | null,
  tabs: [tabOf("u-1"), tabOf("u-2")] as TabShape[],
  sent: [] as Sent[],
  /** Sessions whose open the host refuses, as its gate refuses a row. */
  refuseOpen: new Set<string>(),
  frames: undefined as undefined | { onmessage: Callback },
  events: new Map<string, Callback[]>(),
};

function frame(section: string, body: unknown) {
  host.version += 1;
  return { section, version: host.version, epoch: 1, host_id: HOST, daemon_read: null, body };
}

const shellBody = () => ({ current_screen: host.screen, previous_screen: null, notifications: [] });

function sessionsBody() {
  const row = (id: string, path: string) => ({
    id,
    name: id,
    status: "Running",
    branch_name: `ainb/${id}`,
    workspace_path: path,
    attention: [],
  });
  return {
    workspaces: [
      { name: "a", path: "/a", shell_session: null, sessions: [row("u-1", "/a/one"), row("u-2", "/a/two")] },
      { name: "b", path: "/b", shell_session: null, sessions: [row("u-3", "/b/one")] },
    ],
    selected_session_id: host.selected,
    shell_selected: false,
  };
}

/** Put the reducer on `screen` and frame it, as the host does after a move. */
export function show(screen: string): void {
  host.screen = screen;
  host.frames?.onmessage({ frames: [frame("shell", shellBody())] });
}

/** The host's tab strip changing: `focus` is the tab its answer shows. */
function tabsChanged(tabs: TabShape[], focus: string | null): void {
  host.tabs = tabs;
  for (const handler of host.events.get("terminal_tabs") ?? []) {
    handler({ event: "terminal_tabs", id: 0, payload: { tabs, focus } });
  }
}

function dispatch(intent: Intent): unknown {
  if (!("Command" in intent)) return null;
  const [id, args] = intent.Command;
  const { target, open } = (args ?? {}) as { target?: { session?: string }; open?: boolean };
  host.sent.push({ id, session: target?.session, open });
  if (id === "session_list.select_row") {
    if (host.screen !== "session_list") return { command: id, reason: "it is not in context on this screen" };
    if (open && target?.session !== undefined && host.refuseOpen.has(target.session)) {
      return { command: id, reason: "the session is gone" };
    }
    host.selected = target?.session ?? null;
    host.frames?.onmessage({ frames: [frame("sessions", sessionsBody())] });
    const session = target?.session;
    if (open && session !== undefined && !host.tabs.some((tab) => tab.target.id === session)) {
      const tab = tabOf(session);
      setTimeout(() => tabsChanged([...host.tabs, tab], tab.key), 0);
    }
  }
  const pages: Record<string, string> = {
    "home.config": "config",
    "home.inbox": "inbox",
    "home.sessions": "session_list",
  };
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
        host.frames.onmessage({ frames: [frame("shell", shellBody()), frame("sessions", sessionsBody())] });
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

/** Mount the window as macOS does: its `MAC` is read from the user agent. */
export function pretendMac(): void {
  const userAgent = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/605.1.15";
  Object.defineProperty(globalThis, "navigator", {
    value: { userAgent, platform: "MacIntel", language: navigator.language, hardwareConcurrency: navigator.hardwareConcurrency },
    configurable: true,
    writable: true,
  });
}

/** Mount the whole window over the fake host and wait for its first frames. */
export async function mountWindow(): Promise<void> {
  const root = document.createElement("div");
  root.id = "root";
  document.body.appendChild(root);
  await import("../../src/main.tsx");
  await until(() => document.querySelectorAll(".tab[data-state]").length === host.tabs.length, "the tab strip");
  await until(() => document.querySelector(".sidebar .worktree-card") !== null, "the sidebar's worktrees");
}

/** Wait until `ready` holds, a few event-loop turns at a time. */
export async function until(ready: () => boolean, what: string): Promise<void> {
  for (let turn = 0; turn < 200; turn += 1) {
    if (ready()) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  assert.fail(`timed out waiting for ${what}`);
}

/** Let anything a press sent come back, and any refusal toast. */
export const drain = () => new Promise((resolve) => setTimeout(resolve, 100));

export type Chord = { code: string; key: string; metaKey?: boolean; ctrlKey?: boolean; shiftKey?: boolean };

/** Press `chord` where the keyboard is, as a person does. */
export function press(chord: Chord): KeyboardEvent {
  const event = new window.KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...chord });
  (document.activeElement ?? document.body).dispatchEvent(event);
  return event as unknown as KeyboardEvent;
}

/** Click the tab whose terminal is `sessionId`'s, and wait for it to show. */
export async function showTab(sessionId: string): Promise<void> {
  const tab = [...document.querySelectorAll<HTMLElement>(".tab[data-state]")].find((node) =>
    node.querySelector(".tab-title")?.textContent?.includes(sessionId),
  );
  assert.ok(tab, `${sessionId} has a tab`);
  tab.querySelector<HTMLElement>(".tab-title")!.click();
  await until(() => shownTerminal() === tabOf(sessionId).key, `${sessionId}'s terminal`);
  await drain();
}

export const shownTerminal = () => document.querySelector<HTMLElement>(".terminal[data-tab]:not([hidden])")?.dataset.tab;
export const paletteOpen = () => document.querySelector(".palette") !== null;
export const sidebarShown = () => document.querySelector(".sidebar")?.hasAttribute("hidden") === false;
export const settingsShown = () => document.querySelector(".settings-page") !== null;
/** The class of the element with the keyboard: a string, so a failed
 * assertion prints it rather than walking a whole happy-dom node. */
export const focused = () => document.activeElement?.className ?? "nothing";
export const refusals = () =>
  [...document.querySelectorAll(".toast")].map((toast) => toast.textContent ?? "").filter((text) => text.includes("is not run from the window"));
