// The whole window, mounted over a fake host: with Settings open, a click on
// a terminal tab leaves Settings and shows that tab, as Orca's activation does
// (it switches any page back to the terminal view before it activates the
// worktree). The fake host keeps the reducer's screen and refuses a
// session-list row while that screen is Config, as the real host's screen gate
// does, so a click that sends the row first shows the refusal toast.

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
  if (id === "home.config") host.screen = "config";
  if (id === "home.sessions") host.screen = "session_list";
  if (id === "home.config" || id === "home.sessions") host.frames?.onmessage(shellFrame());
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
        return { tabs: [TAB], focus: null };
      case "subscribe":
        host.frames = args.frames as { onmessage: Callback };
        host.frames.onmessage(shellFrame());
        return HOST;
      case "dispatch":
        return dispatch(args.intent as Intent);
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
const toasts = () => [...document.querySelectorAll(".toast")].map((toast) => toast.textContent ?? "");
const terminal = () => document.querySelector<HTMLElement>(`.terminal[data-tab="${TAB.key}"]`);

test("a tab clicked from Settings closes Settings and shows the tab, with no refusal", async () => {
  const root = document.createElement("div");
  root.id = "root";
  document.body.appendChild(root);
  await import("../../src/main.tsx");

  await until(settingsShown, "the Settings page, the reducer being on Config");
  const title = document.querySelector<HTMLElement>(".tab[data-state] .tab-title");
  assert.ok(title, "the tab strip lists the tab under Settings");
  title.click();

  await until(() => !settingsShown(), "Settings to close");
  await until(() => terminal()?.hidden === false, "the tab's terminal to show");
  // Let any refusal the click drew come back and toast.
  await new Promise((resolve) => setTimeout(resolve, 100));
  assert.deepEqual(
    toasts().filter((text) => text.includes("is not run from the window")),
    [],
    `no refused intent: the window sent ${host.sent.join(", ")}`,
  );
  assert.ok(
    host.sent.indexOf("home.sessions") < host.sent.lastIndexOf("session_list.select_row"),
    `the row is selected after the reducer leaves Config: ${host.sent.join(", ")}`,
  );
});
