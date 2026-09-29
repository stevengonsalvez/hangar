// A terminal's right-click menu, driven as a person drives it: a right-click
// (or the context-menu key) in a mounted pane (`TerminalView`, as the window
// mounts it), a row chosen by click or by arrows and Enter, Esc to close. The
// pane's output arrives through the channel the host writes to, and every
// host call is counted, so a paste is seen going through the host's own
// clipboard read, once, and a right-click is seen never reaching tmux.

import { hostCalls, hostReplies, settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent, createSignal } from "solid-js";
import { render } from "solid-js/web";
import { TerminalView } from "../../src/terminal.tsx";
import type { ByteChannel } from "../../src/transport.ts";

// The pane watches its host's size; happy-dom has the observer, Node does not.
Object.defineProperty(globalThis, "ResizeObserver", {
  value: (window as unknown as { ResizeObserver: unknown }).ResizeObserver,
  configurable: true,
  writable: true,
});

// Every host call, in order: `hostCalls` keeps only the last of each.
const calls: { command: string; args: unknown }[] = [];
const bridge = (
  window as unknown as { __TAURI_INTERNALS__: { invoke(command: string, args?: unknown): Promise<unknown> } }
).__TAURI_INTERNALS__;
const hostInvoke = bridge.invoke;
bridge.invoke = (command, args) => {
  calls.push({ command, args });
  return hostInvoke(command, args);
};
const callsTo = (command: string) => calls.filter((call) => call.command === command).map((call) => call.args);

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  hostCalls.clear();
  hostReplies.clear();
  calls.length = 0;
  document.body.innerHTML = "";
});

/** Let xterm paint: it runs on timers. */
const paint = () => new Promise((done) => setTimeout(done, 50));

/** The class of the element with the keyboard, as a string for a readable failure. */
const focused = () => document.activeElement?.className ?? "nothing";

/** One attached pane with `text` painted into it, its keyboard target focused. */
async function mountPane(text = "hello\r\n", mac = true, active: () => boolean = () => true) {
  let leaves = 0;
  const container = document.createElement("div");
  document.body.appendChild(container);
  cleanup = render(
    () =>
      createComponent(TerminalView, {
        tab: { key: "tmux_app", target: { kind: "tmux", tmux: "tmux_app" }, state: "attached" } as never,
        title: "app",
        get active() {
          return active();
        },
        mac,
        onAccelerator() {},
        onLeave() {
          leaves += 1;
        },
        focusRef() {},
        theme: "dark",
      }),
    container,
  );
  await settle();
  const output = (hostCalls.get("terminal_output") as { bytes: ByteChannel } | undefined)?.bytes;
  assert.ok(output, "the pane opened its output channel");
  output.onmessage(new TextEncoder().encode(text).buffer as ArrayBuffer);
  await paint();
  const terminal = container.querySelector(".xterm-helper-textarea") as HTMLTextAreaElement;
  assert.ok(terminal, "xterm mounted its keyboard target");
  terminal.focus();
  const screen = container.querySelector(".xterm-screen") as HTMLElement;
  const menu = () => container.querySelector(".terminal-menu") as HTMLElement | null;
  const rows = () => [...container.querySelectorAll('.terminal-menu [role="menuitem"]')] as HTMLButtonElement[];
  const row = (label: string) => {
    const found = rows().find((button) => button.textContent?.startsWith(label));
    assert.ok(found, `the menu has a ${label} row`);
    return found;
  };
  const rightClick = async (x = 120, y = 80, target: Element = screen) => {
    const event = new window.MouseEvent("contextmenu", {
      bubbles: true,
      cancelable: true,
      button: 2,
      clientX: x,
      clientY: y,
    });
    target.dispatchEvent(event);
    await settle();
    return event as unknown as MouseEvent;
  };
  const choose = async (label: string) => {
    row(label).click();
    await paint();
  };
  const press = async (target: Element, init: KeyboardEventInit) => {
    const event = new window.KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init });
    target.dispatchEvent(event);
    await settle();
    return event as unknown as KeyboardEvent;
  };
  return { container, terminal, screen, menu, rows, row, rightClick, choose, press, leaves: () => leaves };
}

test("a right-click opens the menu at the pointer, not the webview's, with Orca's rows in order", async () => {
  const pane = await mountPane();
  assert.equal(pane.menu(), null, "closed until asked");
  const event = await pane.rightClick(120, 80);
  assert.equal(event.defaultPrevented, true, "the webview's own menu does not open too");
  const menu = pane.menu();
  assert.ok(menu, "the menu opened");
  assert.equal(menu.style.left, "120px");
  assert.equal(menu.style.top, "80px");
  assert.deepEqual(
    pane.rows().map((button) => button.querySelector("span")?.textContent),
    ["Copy", "Select All", "Paste", "Find…"],
  );
  assert.equal(document.activeElement, pane.rows()[0], "the first row has the keyboard");
  await pane.rightClick(300, 200);
  assert.equal(pane.menu()?.style.left, "300px", "a second right-click moves it to the new point");
});

test("Copy with a selection writes it to the clipboard; Select All selects the whole buffer", async () => {
  const pane = await mountPane("hello\r\nworld\r\n");
  await pane.rightClick();
  await pane.choose("Select All");
  assert.equal(pane.menu(), null, "a chosen row closes the menu");
  assert.equal(focused(), "xterm-helper-textarea", "and hands the keyboard back to the terminal");
  await pane.rightClick();
  await pane.choose("Copy");
  const written = callsTo("clipboard_write") as { text: string }[];
  assert.equal(written.length, 1);
  assert.match(written[0].text, /hello\s+world/);
  assert.equal(focused(), "xterm-helper-textarea");
});

test("Copy with nothing selected writes nothing", async () => {
  const pane = await mountPane();
  await pane.rightClick();
  await pane.choose("Copy");
  assert.deepEqual(callsTo("clipboard_write"), []);
  assert.equal(pane.menu(), null);
});

test("Paste reads the clipboard through the host's in-view check, once, and types it through xterm", async () => {
  const pane = await mountPane();
  hostReplies.set("clipboard_read", "echo pasted");
  await pane.rightClick();
  await pane.choose("Paste");
  assert.deepEqual(callsTo("clipboard_read"), [{ key: "tmux_app" }], "one read, for this tab");
  const typed = (callsTo("terminal_input") as { data: string }[]).map((input) => input.data).join("");
  assert.equal(typed, "echo pasted", "the text reached the shell");
});

test("Paste with an empty clipboard, or one the host refused, types nothing", async () => {
  const pane = await mountPane();
  hostReplies.set("clipboard_read", "");
  await pane.rightClick();
  await pane.choose("Paste");
  assert.equal(callsTo("clipboard_read").length, 1);
  assert.deepEqual(callsTo("terminal_input"), []);
});

test("the right button never reaches xterm, so xterm cannot report it to tmux; the left one still does", async () => {
  // ainb's tmux sessions run with tmux's mouse on, so xterm reports every
  // button it hears to tmux, and tmux answers a right-click with its own
  // menu. happy-dom lays out no cells, so xterm sends no report here; what
  // is checked is that xterm's own element never hears the right button.
  const pane = await mountPane();
  const heard: string[] = [];
  const xterm = pane.container.querySelector(".xterm") as Element;
  for (const type of ["mousedown", "mouseup", "contextmenu"]) {
    xterm.addEventListener(type, (event) => heard.push(`${type}:${(event as MouseEvent).button}`));
  }
  const mouse = (type: string, button: number) =>
    pane.screen.dispatchEvent(
      new window.MouseEvent(type, { bubbles: true, cancelable: true, button, clientX: 20, clientY: 10 }),
    );
  mouse("mousedown", 2);
  mouse("mouseup", 2);
  await pane.rightClick(20, 10);
  assert.deepEqual(heard, [], "xterm heard nothing of the right-click");
  assert.ok(pane.menu(), "and the menu opened");
  await pane.press(pane.rows()[0], { key: "Escape" });
  mouse("mousedown", 0);
  mouse("mouseup", 0);
  assert.deepEqual(
    heard,
    ["mousedown:0", "mouseup:0"],
    "the left button still reaches it, so the check above can fail",
  );
});

test("a right-click in the pane's margin opens the menu too; one in the find bar keeps the webview's", async () => {
  const pane = await mountPane();
  const margin = pane.container.querySelector(".terminal") as Element;
  const event = await pane.rightClick(2, 2, margin);
  assert.equal(event.defaultPrevented, true);
  assert.ok(pane.menu());
  await pane.press(pane.rows()[0], { key: "Escape" });
  await pane.rightClick();
  await pane.choose("Find");
  const query = pane.container.querySelector(".terminal-search-query") as Element;
  const inBar = await pane.rightClick(400, 10, query);
  assert.equal(inBar.defaultPrevented, false, "the query field keeps its own cut, copy and paste menu");
  assert.equal(pane.menu(), null);
});

test("Find opens the pane's find bar with its query focused", async () => {
  const pane = await mountPane();
  await pane.rightClick();
  await pane.choose("Find");
  assert.ok(pane.container.querySelector(".terminal-search"), "the find bar opened");
  assert.equal(focused(), "terminal-search-query");
  assert.equal(pane.menu(), null);
});

test("Esc closes the menu and gives the keyboard back to the terminal, without leaving the pane", async () => {
  const pane = await mountPane();
  const seen: string[] = [];
  const spy = (event: Event) => seen.push((event as KeyboardEvent).key);
  window.addEventListener("keydown", spy);
  try {
    await pane.rightClick();
    await pane.press(pane.rows()[0], { key: "ArrowDown" });
    const event = await pane.press(pane.rows()[1], { key: "Escape", code: "Escape" });
    assert.equal(pane.menu(), null);
    assert.equal(focused(), "xterm-helper-textarea");
    assert.equal(event.defaultPrevented, true);
    assert.deepEqual(seen, [], "the window's own key handling never saw the menu's keys");
    await pane.rightClick();
    await pane.press(pane.rows()[0], { key: "Escape", code: "Escape" });
    assert.equal(pane.leaves(), 0, "two quick Escs through the menu are not the pane's Esc Esc");
  } finally {
    window.removeEventListener("keydown", spy);
  }
});

test("Shift+F10 and the context-menu key open it from the keyboard; arrows walk the rows, Enter runs one", async () => {
  const pane = await mountPane();
  const shiftF10 = await pane.press(pane.terminal, { key: "F10", code: "F10", shiftKey: true });
  assert.ok(pane.menu(), "Shift+F10 opened the menu");
  assert.equal(shiftF10.defaultPrevented, true);
  assert.deepEqual(callsTo("terminal_input"), [], "the shell does not also get Shift+F10");
  const [copy, selectAll, , find] = pane.rows();
  await pane.press(copy, { key: "ArrowDown" });
  assert.equal(document.activeElement, selectAll);
  await pane.press(selectAll, { key: "ArrowUp" });
  await pane.press(copy, { key: "ArrowUp" });
  assert.equal(document.activeElement, find, "up from the first row wraps to the last");
  await pane.press(find, { key: "Escape" });
  assert.equal(pane.menu(), null);

  await pane.press(pane.terminal, { key: "ContextMenu", code: "ContextMenu" });
  assert.ok(pane.menu(), "the context-menu key opened it");
  await pane.press(pane.rows()[0], { key: "ArrowDown" });
  await pane.press(document.activeElement as Element, { key: "Enter" });
  assert.equal(pane.menu(), null, "Enter ran Select All and closed the menu");
  await pane.rightClick();
  await pane.choose("Copy");
  assert.match((callsTo("clipboard_write")[0] as { text: string }).text, /hello/, "Select All had run");
});

test("Home and End jump to the ends, Space runs a row, Tab closes back to the terminal", async () => {
  const pane = await mountPane();
  await pane.rightClick();
  const all = pane.rows();
  await pane.press(all[0], { key: "End" });
  assert.equal(document.activeElement, all[3]);
  await pane.press(all[3], { key: "Home" });
  assert.equal(document.activeElement, all[0]);
  await pane.press(all[0], { key: "Tab" });
  assert.equal(pane.menu(), null);
  assert.equal(focused(), "xterm-helper-textarea");
  await pane.rightClick();
  await pane.press(pane.rows()[0], { key: "ArrowDown" });
  await pane.press(document.activeElement as Element, { key: " ", code: "Space" });
  assert.equal(pane.menu(), null, "Space ran Select All");
  await pane.rightClick();
  await pane.choose("Copy");
  assert.match((callsTo("clipboard_write")[0] as { text: string }).text, /hello/);
});

test("a click outside closes the menu and leaves the focus where the click put it", async () => {
  const pane = await mountPane();
  await pane.rightClick();
  assert.ok(pane.menu(), "open before the click");
  const elsewhere = document.createElement("button");
  document.body.appendChild(elsewhere);
  elsewhere.dispatchEvent(new window.PointerEvent("pointerdown", { bubbles: true }));
  elsewhere.focus();
  await settle();
  assert.equal(pane.menu(), null);
  assert.equal(document.activeElement, elsewhere);
});

test("focus moving to another field closes the menu", async () => {
  const pane = await mountPane();
  await pane.rightClick();
  const field = document.createElement("input");
  document.body.appendChild(field);
  field.focus();
  await settle();
  assert.equal(pane.menu(), null);
  assert.equal(document.activeElement, field);
});

test("the window losing focus closes the menu and leaves the terminal the keyboard", async () => {
  const pane = await mountPane();
  await pane.rightClick();
  window.dispatchEvent(new window.Event("blur"));
  await settle();
  assert.equal(pane.menu(), null);
  assert.equal(focused(), "xterm-helper-textarea");
});

test("a tab switch that hides the pane closes its menu", async () => {
  const [shown, setShown] = createSignal(true);
  const pane = await mountPane("hello\r\n", true, shown);
  await pane.rightClick();
  assert.ok(pane.menu());
  setShown(false);
  await settle();
  assert.equal(pane.menu(), null);
});

test("off macOS the rows show the Ctrl+Shift chords the pane binds", async () => {
  const pane = await mountPane("hello\r\n", false);
  await pane.rightClick();
  assert.equal(pane.row("Copy").querySelector("kbd")?.textContent, "Ctrl+Shift+C");
  assert.equal(pane.row("Paste").querySelector("kbd")?.textContent, "Ctrl+Shift+V");
  assert.equal(pane.row("Find").querySelector("kbd")?.textContent, "Ctrl+Shift+F");
  assert.equal(
    pane.row("Select All").querySelector("kbd"),
    null,
    "no chord, no label, as Orca hides an unassigned one",
  );
});
