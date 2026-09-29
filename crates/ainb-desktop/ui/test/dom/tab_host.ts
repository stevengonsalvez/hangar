// A fake host that owns the terminal tab strip as the real one does
// (`terminal_tabs`, `terminal_close`, the tabs in view and the clipboard gate
// behind them), installed as the
// window's Tauri bridge, and the readers the split-pane window tests share:
// which tabs each pane's strip holds, which terminals show, what was sent.
//
// Imported by a test file before it imports `main.tsx`; set `host.tabs` first
// to choose the strip the window's first `terminal_tabs` call answers with.

import "./window.ts";

import assert from "node:assert/strict";

type Callback = (payload: unknown) => void;

/** `tabs` as the window receives them: fresh objects on every answer, as a
 * strip crossing the real IPC is. Handing the same objects back each time
 * would hide a view keyed by tab object, which remounts on every frame. */
function copy(tabs: readonly TabView[]): TabView[] {
  return JSON.parse(JSON.stringify(tabs));
}
export type TabView = { key: string; target: { kind: "session"; id: string; tmux: string }; state: "attached" };

const HOST = "host-1";
export const tab = (name: string): TabView => ({ key: `tmux_${name}`, target: { kind: "session", id: `u-${name}`, tmux: `tmux_${name}` }, state: "attached" });

/** The fake host: its tab strip, and every call the window made, in order. */
export const host = {
  tabs: ["a", "b", "c", "d"].map(tab),
  /** The tabs in view: the set the panes named last, or, until they name
   * one, the tab sized last. The real host answers a clipboard read for
   * these only (`Terminals::showing`). */
  inView: [] as string[],
  named: false,
  calls: [] as { command: string; args: Record<string, unknown> }[],
  events: new Map<string, Callback[]>(),
};

/** The host's strip changing on its own schedule, focusing `focus` or not. */
export function strip(focus: string | null = null): void {
  for (const handler of host.events.get("terminal_tabs") ?? []) {
    handler({ event: "terminal_tabs", id: 0, payload: { tabs: copy(host.tabs), focus } });
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
    host.calls.push({ command, args });
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
        return { tabs: copy(host.tabs), focus: null };
      case "terminal_close":
        // The real host drops the tab and answers with the strip, as here.
        host.tabs = host.tabs.filter((one) => one.key !== args.key);
        queueMicrotask(() => strip());
        return null;
      case "subscribe":
        return HOST;
      case "terminal_visible":
        host.inView = [...(args.keys as string[])];
        host.named = true;
        return true;
      case "terminal_resize":
        if (!host.named) host.inView = [args.key as string];
        return null;
      case "clipboard_read":
        return host.inView.includes(args.key as string) ? "pasted" : "";
      default:
        return null;
    }
  },
};

// The window's own globals happy-dom does not hand Node by default.
for (const name of ["requestAnimationFrame", "cancelAnimationFrame", "ResizeObserver", "getComputedStyle", "localStorage", "PointerEvent", "KeyboardEvent", "MouseEvent"]) {
  const value = (window as unknown as Record<string, unknown>)[name];
  Object.defineProperty(globalThis, name, {
    value: typeof value === "function" && !/^[A-Z]/.test(name) ? (value as () => unknown).bind(window) : value,
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
export async function until(ready: () => boolean, what: string): Promise<void> {
  for (let turn = 0; turn < 200; turn += 1) {
    if (ready()) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  assert.fail(`timed out waiting for ${what}`);
}
export const tick = () => new Promise((resolve) => setTimeout(resolve, 30));

/** Each pane as `id:tabs*shown`, in the order the window drew them, with
 * `!` on the focused one. Tab keys are shortened to their session name. */
export function panes(): string[] {
  return [...document.querySelectorAll<HTMLElement>(".pane-group")].map((group) => {
    const keys = [...group.querySelectorAll<HTMLElement>(".tab[data-key]")].map((one) => one.dataset.key!.slice(5));
    const shown = group.querySelector<HTMLElement>(".tab.active[data-key]")?.dataset.key?.slice(5) ?? "";
    return `${group.dataset.group}:${keys.join(",")}*${shown}${group.hasAttribute("data-focused") ? "!" : ""}`;
  });
}

/** The tabs the host holds in view, by session name. */
export function inView(): string[] {
  return host.inView.map((key) => key.slice(5)).sort();
}

/** The terminals on screen, by session name. */
export function visible(): string[] {
  // `[data-tab]`: xterm's own element carries the class `terminal` too.
  return [...document.querySelectorAll<HTMLElement>(".pane-slot:not([hidden]) .terminal[data-tab]:not([hidden])")]
    .map((terminal) => terminal.dataset.tab!.slice(5))
    .sort();
}

export const group = (id: string) => document.querySelector<HTMLElement>(`.pane-group[data-group="${id}"]`)!;
export const tabEl = (name: string) => document.querySelector<HTMLElement>(`.tab[data-key="tmux_${name}"]`)!;
export const stored = () => JSON.parse(localStorage.getItem("ainb.layout") ?? "null");
export const closes = () => host.calls.filter((call) => call.command === "terminal_close").map((call) => (call.args.key as string).slice(5));
export const selected = () =>
  host.calls
    .filter((call) => call.command === "dispatch")
    .map((call) => (call.args.intent as { Command: [string, { target: { session: string } }] }).Command)
    .filter(([id]) => id === "session_list.select_row")
    .map(([, args]) => args.target.session);

/** Right-click `name`'s tab and choose the menu entry `selector` names. */
export async function fromMenu(name: string, selector: string): Promise<void> {
  tabEl(name).dispatchEvent(new window.MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 10, clientY: 10 }));
  await until(() => document.querySelector(`.pane-menu ${selector}`) !== null, `the menu entry ${selector}`);
  document.querySelector<HTMLElement>(`.pane-menu ${selector}`)!.click();
  await tick();
}

/** The panes laid out 1000 x 600 at the window's corner, as a real page
 * would measure them (happy-dom lays nothing out). */
export function measure(): void {
  const box = document.querySelector<HTMLElement>(".panes")!;
  box.getBoundingClientRect = () => ({ left: 0, top: 0, width: 1000, height: 600, right: 1000, bottom: 600, x: 0, y: 0, toJSON() {} }) as DOMRect;
}
export function pointer(target: EventTarget, type: string, x: number, y: number): void {
  target.dispatchEvent(new window.PointerEvent(type, { bubbles: true, cancelable: true, button: 0, clientX: x, clientY: y, pointerId: 1 }));
}

