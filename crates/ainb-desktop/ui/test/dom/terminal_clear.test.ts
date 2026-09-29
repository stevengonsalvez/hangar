// Cmd+K (Ctrl+Shift+K off macOS) in a mounted pane (`TerminalView`, as the
// window mounts it) clears its scrollback, Orca's `terminal.clear`. The pane
// answers it itself, as it does copy and paste: the window is not asked, and
// the shell gets no byte. Plain Ctrl+K stays the shell's kill-line.

import { hostCalls, settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent } from "solid-js";
import { render } from "solid-js/web";
import { TerminalView } from "../../src/terminal.tsx";
import type { Accelerator } from "../../src/tabs.ts";
import type { ByteChannel } from "../../src/transport.ts";

Object.defineProperty(globalThis, "ResizeObserver", {
  value: (window as unknown as { ResizeObserver: unknown }).ResizeObserver,
  configurable: true,
  writable: true,
});

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  hostCalls.clear();
  document.body.innerHTML = "";
});

/** Let xterm paint: it writes and renders on timers. */
const paint = () => new Promise((done) => setTimeout(done, 50));

type Chord = { code: string; key: string; keyCode: number; metaKey?: boolean; ctrlKey?: boolean; shiftKey?: boolean };

/** One attached pane on `mac` or not, with a screenful and more painted. */
async function mountPane(mac: boolean) {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const asked: Accelerator[] = [];
  cleanup = render(
    () =>
      createComponent(TerminalView, {
        tab: { key: "tmux_app", target: { kind: "session", id: "s-1", tmux: "tmux_app" }, state: "attached" } as never,
        title: "app",
        active: true,
        mac,
        onAccelerator: (shell: Accelerator) => asked.push(shell),
        onLeave() {},
        focusRef() {},
        theme: "dark",
      }),
    container,
  );
  await settle();
  const output = (hostCalls.get("terminal_output") as { bytes: ByteChannel } | undefined)?.bytes;
  assert.ok(output, "the pane opened its output channel");
  const lines = Array.from({ length: 40 }, (_, n) => `old line ${n}`).join("\r\n") + "\r\n$ ";
  output.onmessage(new TextEncoder().encode(lines).buffer as ArrayBuffer);
  await paint();
  const terminal = container.querySelector(".xterm-helper-textarea") as HTMLTextAreaElement;
  assert.ok(terminal, "xterm mounted its keyboard target");
  terminal.focus();
  const press = async (chord: Chord) => {
    const event = new window.KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...chord });
    // happy-dom leaves `keyCode` 0; xterm decides by it.
    Object.defineProperty(event, "keyCode", { value: chord.keyCode });
    terminal.dispatchEvent(event);
    await paint();
    return event;
  };
  // The find bar counts matches in the whole buffer, scrollback included:
  // what a person would use to check the old output is gone.
  const matches = async (query: string) => {
    await press(mac ? { code: "KeyF", key: "f", keyCode: 70, metaKey: true } : { code: "KeyF", key: "F", keyCode: 70, ctrlKey: true, shiftKey: true });
    const field = container.querySelector(".terminal-search-query") as HTMLInputElement;
    field.value = query;
    field.dispatchEvent(new window.Event("input", { bubbles: true }));
    await paint();
    const count = container.querySelector(".terminal-search-count")?.textContent;
    field.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    await paint();
    terminal.focus();
    return count;
  };
  /** The rows xterm draws: the viewport a person sees. */
  const shown = () => container.querySelector(".xterm-rows")?.textContent ?? "";
  /** The shell writing `text` to the pane, through the host's channel. */
  const write = async (text: string) => {
    output.onmessage(new TextEncoder().encode(text).buffer as ArrayBuffer);
    await paint();
  };
  return { press, matches, shown, write, asked };
}

for (const mac of [true, false]) {
  const chord: Chord = mac
    ? { code: "KeyK", key: "k", keyCode: 75, metaKey: true }
    : { code: "KeyK", key: "K", keyCode: 75, ctrlKey: true, shiftKey: true };
  const name = mac ? "Cmd+K" : "Ctrl+Shift+K";

  test(`${name} clears the pane's scrollback, answered by the pane itself`, async () => {
    const pane = await mountPane(mac);
    assert.match((await pane.matches("old line")) ?? "", /\/40$/, "forty old lines before the chord");
    assert.match(pane.shown(), /old line 39/, "the last of them on screen");
    hostCalls.delete("terminal_input");
    const event = await pane.press(chord);
    assert.equal(event.defaultPrevented, true, "the chord was taken");
    assert.doesNotMatch(pane.shown(), /old line/, "none left on screen");
    assert.match(pane.shown(), /\$/, "the prompt stays");
    // The find bar's line cache outlives a clear until the pane's next output
    // (xterm's `SearchLineCache`, as in Orca): search once the shell has
    // written again, as a person would after a clear.
    await pane.write("\r\n$ ");
    assert.equal(await pane.matches("old lin"), "No results", "and none in the scrollback");
    assert.deepEqual(pane.asked, [], "the window was not asked");
    assert.equal(hostCalls.get("terminal_input"), undefined, "and the shell got no byte");
  });
}

test("plain Ctrl+K stays the shell's kill-line on either platform", async () => {
  for (const mac of [true, false]) {
    const pane = await mountPane(mac);
    await pane.press({ code: "KeyK", key: "k", keyCode: 75, ctrlKey: true });
    assert.deepEqual(pane.asked, []);
    assert.equal((hostCalls.get("terminal_input") as { data: string } | undefined)?.data, "\x0b");
    assert.match(pane.shown(), /old line 39/, "nothing cleared");
    assert.match((await pane.matches("old lin")) ?? "", /\/40$/, "nothing cleared from the scrollback");
    cleanup?.();
    cleanup = undefined;
    hostCalls.clear();
  }
});
