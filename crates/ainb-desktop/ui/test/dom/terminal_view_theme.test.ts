// A theme switch on an open terminal moves its contrast floor with its
// palette: Light holds text to 4.5:1 and Dark to 3:1, as Orca's panes do. The
// pane is mounted as the window mounts it (`TerminalView`), its output is fed
// through the channel the host would write to, the theme is switched while it
// stays open, and the colours xterm drew are read back.

import { hostCalls, settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent, createSignal } from "solid-js";
import { render } from "solid-js/web";
import { TerminalView } from "../../src/terminal.tsx";
import { TERMINAL_THEMES, type Theme } from "../../src/theme/theme.ts";
import type { ByteChannel } from "../../src/transport.ts";

// The pane watches its host's size; happy-dom has the observer, Node does not.
Object.defineProperty(globalThis, "ResizeObserver", {
  value: (window as unknown as { ResizeObserver: unknown }).ResizeObserver,
  configurable: true,
  writable: true,
});

/** WCAG 2 relative luminance of a `#rrggbb` colour. */
function luminance(hex: string): number {
  const channel = (at: number) => {
    const value = parseInt(hex.slice(at, at + 2), 16) / 255;
    return value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5);
}

/** WCAG 2 contrast ratio between two `#rrggbb` colours, 1 to 21. */
function contrast(a: string, b: string): number {
  const [light, dark] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (light + 0.05) / (dark + 0.05);
}

/** A span's inline text colour as `#rrggbb` (the DOM renderer escapes the `#`). */
function color(span: Element): string | undefined {
  const style = span.getAttribute("style") ?? "";
  return /(?:^|;)color:\\?(#[0-9a-f]{6})/i.exec(style)?.[1]?.toLowerCase();
}

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  document.body.innerHTML = "";
});

/** Let xterm paint: the DOM renderer draws on the next animation frame. */
const paint = () => new Promise((done) => setTimeout(done, 50));

/**
 * Mount one attached terminal pane in `start`, paint `bytes` into it through
 * its output channel, and return a switch for the window's theme plus a
 * reader for the span holding `text`.
 */
async function mountPane(start: Theme, bytes: string) {
  const [theme, setTheme] = createSignal<Theme>(start);
  const container = document.createElement("div");
  document.body.appendChild(container);
  cleanup = render(
    () =>
      createComponent(TerminalView, {
        tab: { key: "tmux_app", target: { kind: "session", id: "s-1", tmux: "tmux_app" }, state: "attached" } as never,
        title: "app",
        active: true,
        mac: false,
        onAccelerator() {},
        onLeave() {},
        focusRef() {},
        get theme() {
          return theme();
        },
      }),
    container,
  );
  await settle();
  const output = (hostCalls.get("terminal_output") as { bytes: ByteChannel } | undefined)?.bytes;
  assert.ok(output, "the pane opened its output channel");
  output.onmessage(new TextEncoder().encode(bytes).buffer as ArrayBuffer);
  await paint();
  const span = (text: string) => {
    for (const candidate of container.querySelectorAll(".xterm-rows span")) {
      if (candidate.textContent?.trim() === text) return candidate;
    }
    assert.fail(`"${text}" is drawn: ${container.querySelector(".xterm-rows")?.innerHTML}`);
  };
  return {
    span,
    async switchTo(next: Theme) {
      setTheme(next);
      await settle();
      await paint();
    },
  };
}

test("dark to light on an open pane: 24-bit white is lifted to Light's 4.5:1, not Dark's 3:1", async () => {
  // Claude Code's dark theme sends bold headings as 24-bit white.
  const pane = await mountPane("dark", "\x1b[1;38;2;255;255;255mHeading\x1b[0m");
  await pane.switchTo("light");
  const drawn = color(pane.span("Heading"));
  const background = TERMINAL_THEMES.light.background;
  assert.ok(drawn, "the heading carries a corrected colour");
  assert.ok(contrast(drawn, background) >= 4.5, `${drawn} on ${background} reads at 4.5:1`);
});

test("light to dark on an open pane: a grey that clears 3:1 is drawn as sent, not lifted to 4.5:1", async () => {
  const sent = "#6e6e6e";
  const background = TERMINAL_THEMES.dark.background;
  const ratio = contrast(sent, background);
  // Between the two floors, so only the floor the pane holds decides its colour.
  assert.ok(ratio > 3 && ratio < 4.5, `${sent} on ${background} is ${ratio.toFixed(2)}:1`);
  const pane = await mountPane("light", "\x1b[38;2;110;110;110mgrey\x1b[0m");
  await pane.switchTo("dark");
  assert.equal(color(pane.span("grey")), sent);
});
