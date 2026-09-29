// A link click in a mounted pane (`TerminalView`, as the window mounts it):
// the pane's output arrives through the channel the host writes to, xterm's
// own link providers find the URLs in its buffer, and a Ctrl+click (Cmd+click
// on macOS) on one asks the host to open it (`open_url`). The webview opens
// nothing itself. happy-dom lays nothing out, so where xterm would hover the
// link under the pointer the test hovers the link xterm found, then presses
// and releases the mouse on the pane's screen as a person does.

import { hostCalls, settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent } from "solid-js";
import { render } from "solid-js/web";
import { Terminal, type ILink, type ILinkProvider } from "@xterm/xterm";
import { TerminalView } from "../../src/terminal.tsx";
import type { ByteChannel } from "../../src/transport.ts";

// The pane watches its host's size; happy-dom has the observer, Node does not.
Object.defineProperty(globalThis, "ResizeObserver", {
  value: (window as unknown as { ResizeObserver: unknown }).ResizeObserver,
  configurable: true,
  writable: true,
});

// Every provider a pane registers, and the terminal it registered on.
const providers: ILinkProvider[] = [];
let terminal: Terminal | undefined;
const register = Terminal.prototype.registerLinkProvider;
Terminal.prototype.registerLinkProvider = function (this: Terminal, provider: ILinkProvider) {
  providers.push(provider);
  terminal = this;
  return register.call(this, provider);
};

// Nothing on any path may open a window of its own.
const opened: unknown[] = [];
(window as unknown as { open: (...args: unknown[]) => null }).open = (...args) => {
  opened.push(args);
  return null;
};

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  providers.length = 0;
  terminal = undefined;
  hostCalls.clear();
  document.body.innerHTML = "";
  assert.deepEqual(opened, [], "window.open was called");
});

const paint = () => new Promise((done) => setTimeout(done, 50));

/** Mount one attached pane and paint `bytes` into it through its output channel. */
async function mountPane(bytes: string, mac = false) {
  const container = document.createElement("div");
  document.body.appendChild(container);
  cleanup = render(
    () =>
      createComponent(TerminalView, {
        tab: { key: "tmux_app", target: { kind: "session", id: "s-1", tmux: "tmux_app" }, state: "attached" } as never,
        title: "app",
        active: true,
        mac,
        onAccelerator() {},
        onLeave() {},
        focusRef() {},
        theme: "dark",
      }),
    container,
  );
  await settle();
  const output = (hostCalls.get("terminal_output") as { bytes: ByteChannel } | undefined)?.bytes;
  assert.ok(output, "the pane opened its output channel");
  output.onmessage(new TextEncoder().encode(bytes).buffer as ArrayBuffer);
  await paint();
  return container;
}

/** The link xterm's providers find on buffer row `y` (1-based) at cell `x`, the first provider's winning as xterm's own hover does. */
async function linkAt(x: number, y: number): Promise<ILink> {
  for (const provider of providers) {
    const links = await new Promise<ILink[] | undefined>((done) => provider.provideLinks(y, done));
    const link = links?.find(
      ({ range }) => range.start.y <= y && y <= range.end.y && range.start.x <= x && x <= range.end.x,
    );
    if (link) return link;
  }
  assert.fail(`no link at ${x},${y} from ${providers.length} providers`);
}

const mouse = (type: string, keys: MouseEventInit) =>
  new (window as unknown as { MouseEvent: typeof MouseEvent }).MouseEvent(type, { button: 0, bubbles: true, cancelable: true, ...keys });

/**
 * Hover `link` as xterm does under the pointer, then press and release on the
 * pane's screen with `keys` held. Returns the events xterm itself received.
 */
function click(container: Element, link: ILink, keys: MouseEventInit): string[] {
  link.hover?.(mouse("mousemove", {}), link.text);
  const screen = container.querySelector(".xterm-screen");
  assert.ok(screen, "xterm drew its screen");
  const reached: string[] = [];
  const note = (event: Event) => reached.push(event.type);
  screen.addEventListener("mousedown", note);
  screen.addEventListener("mouseup", note);
  screen.dispatchEvent(mouse("mousedown", keys));
  screen.dispatchEvent(mouse("mouseup", keys));
  return reached;
}

test("Ctrl+click on a URL asks the host to open it, and the click stops there", async () => {
  const pane = await mountPane("build done: see https://example.com/runs/42?tab=logs for details\r\n");
  const link = await linkAt(20, 1);
  assert.equal(link.text, "https://example.com/runs/42?tab=logs");
  const reached = click(pane, link, { ctrlKey: true });
  assert.deepEqual(hostCalls.get("open_url"), { url: "https://example.com/runs/42?tab=logs" });
  assert.deepEqual(reached, [], "xterm, and through it tmux, never saw the link click");
  assert.equal(pane.querySelector(".terminal")?.getAttribute("title"), link.text, "the hover names the target");
});

test("Cmd+click is the link click on macOS", async () => {
  const pane = await mountPane("see https://example.com/mac\r\n", true);
  const link = await linkAt(8, 1);
  click(pane, link, { ctrlKey: true });
  assert.equal(hostCalls.has("open_url"), false, "Ctrl+click is the pane's on macOS");
  click(pane, link, { metaKey: true });
  assert.deepEqual(hostCalls.get("open_url"), { url: "https://example.com/mac" });
});

test("a bare click on a URL stays the pane's: the host is not asked", async () => {
  const pane = await mountPane("see https://example.com/a\r\n");
  const link = await linkAt(8, 1);
  const reached = click(pane, link, {});
  assert.equal(hostCalls.has("open_url"), false);
  assert.deepEqual(reached, ["mousedown", "mouseup"], "xterm had the click");
});

test("once the pointer leaves a link, a Ctrl+click opens nothing", async () => {
  const pane = await mountPane("see https://example.com/a\r\n");
  const link = await linkAt(8, 1);
  link.hover?.(mouse("mousemove", {}), link.text);
  link.leave?.(mouse("mousemove", {}), link.text);
  const screen = pane.querySelector(".xterm-screen")!;
  screen.dispatchEvent(mouse("mousedown", { ctrlKey: true }));
  screen.dispatchEvent(mouse("mouseup", { ctrlKey: true }));
  assert.equal(hostCalls.has("open_url"), false);
  assert.equal(pane.querySelector(".terminal")?.hasAttribute("title"), false);
});

test("a URL a panel wrapped over three rows opens whole from its middle row", async () => {
  const url = "https://claude.ai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&response_type=code";
  const panel = (text: string) => `│ ${text.padEnd(46)} │\r\n`;
  const pane = await mountPane(
    panel("Open this link to sign in:") + panel(url.slice(0, 46)) + panel(url.slice(46, 92)) + panel(url.slice(92)),
  );
  // Its first row too, where the addon alone would have linked only that row.
  assert.equal((await linkAt(10, 2)).text, url);
  const link = await linkAt(10, 3);
  assert.equal(link.text, url);
  click(pane, link, { ctrlKey: true });
  assert.deepEqual(hostCalls.get("open_url"), { url });
});

test("an OSC 8 hyperlink opens through the host too, not xterm's own window.open", async () => {
  const pane = await mountPane("\x1b]8;;https://example.com/pr/7\x07PR 7\x1b]8;;\x07\r\n");
  const handler = terminal?.options.linkHandler;
  assert.ok(handler?.hover, "the pane handles OSC 8 links itself");
  const range = { start: { x: 1, y: 1 }, end: { x: 4, y: 1 } };
  // What xterm hands the handler for the text "PR 7": the target, not the text.
  const link: ILink = {
    text: "https://example.com/pr/7",
    range,
    activate: (event, text) => handler.activate(event, text, range),
    hover: (event, text) => handler.hover!(event, text, range),
  };
  click(pane, link, { ctrlKey: true });
  assert.deepEqual(hostCalls.get("open_url"), { url: "https://example.com/pr/7" });
  assert.equal(pane.querySelector(".terminal")?.getAttribute("title"), "https://example.com/pr/7");
});
