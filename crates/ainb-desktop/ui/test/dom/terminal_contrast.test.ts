// The Light terminal stays legible when an agent CLI paints in its own dark
// theme. Claude Code in a dark theme sends bold headings as 24-bit white and
// its prompt lines on a 24-bit rgb(55, 55, 55) bar with the default
// foreground; no palette slot reaches those cells, so only xterm's
// `minimumContrastRatio` can. These tests render a real xterm (its DOM
// renderer, in happy-dom) with the options the terminal pane uses and read
// back the colours it drew.

import "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
// `npm run test:dom` resolves xterm to its ES module build (`solid-dom.mjs`),
// which has named exports only, the same way `terminal.tsx` imports it.
import { Terminal } from "@xterm/xterm";
import { TERMINAL_THEMES, terminalAppearance, type Theme } from "../../src/theme/theme.ts";

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

/** A span's inline colour as `#rrggbb` (the DOM renderer escapes the `#`). */
function inline(span: Element, property: "color" | "background-color"): string | undefined {
  const style = span.getAttribute("style") ?? "";
  return new RegExp(`(?:^|;)${property}:\\\\?(#[0-9a-f]{6})`, "i").exec(style)?.[1]?.toLowerCase();
}

let dispose: (() => void) | undefined;
afterEach(() => {
  dispose?.();
  dispose = undefined;
  document.body.innerHTML = "";
});

/** Render `bytes` in a terminal built as the pane builds one; its spans by text. */
async function render(theme: Theme, bytes: string): Promise<Map<string, Element>> {
  const host = document.createElement("div");
  document.body.appendChild(host);
  const term = new Terminal({ cols: 60, rows: 3, ...terminalAppearance(theme) });
  dispose = () => term.dispose();
  term.open(host as unknown as HTMLElement);
  await new Promise<void>((done) => term.write(bytes, done));
  // The DOM renderer paints on the next animation frame.
  await new Promise((done) => setTimeout(done, 50));
  const spans = new Map<string, Element>();
  for (const span of host.querySelectorAll(".xterm-rows span")) {
    const text = span.textContent?.trim();
    if (text) spans.set(text, span);
  }
  return spans;
}

// Claude Code's dark theme, as it reaches the pane: a bold 24-bit white
// heading, then a prompt line on its dark `userMessageBackground` in the
// terminal's default foreground.
const CLAUDE_DARK_THEME = "\x1b[1;38;2;255;255;255mHeading\x1b[0m \x1b[48;2;55;55;55mprompt\x1b[0m";

test("light: a bold 24-bit white heading is drawn dark enough to read on white", async () => {
  const spans = await render("light", CLAUDE_DARK_THEME);
  const heading = spans.get("Heading");
  assert.ok(heading, "the heading is drawn");
  const color = inline(heading, "color");
  assert.ok(color, `the heading carries a corrected colour: ${heading.getAttribute("style")}`);
  assert.ok(
    contrast(color, TERMINAL_THEMES.light.background) >= 4.5,
    `${color} on ${TERMINAL_THEMES.light.background} reads at 4.5:1`,
  );
});

test("light: bold ANSI bright white draws from the palette, readable on white", async () => {
  const spans = await render("light", "\x1b[1;97mBold\x1b[0m \x1b[37mwhite\x1b[0m");
  const bold = spans.get("Bold");
  const white = spans.get("white");
  assert.ok(bold && white, "both cells are drawn");
  assert.ok(bold.classList.contains("xterm-fg-15"), bold.outerHTML);
  assert.ok(white.classList.contains("xterm-fg-7"), white.outerHTML);
  const background = TERMINAL_THEMES.light.background;
  for (const [span, slot] of [[bold, TERMINAL_THEMES.light.brightWhite], [white, TERMINAL_THEMES.light.white]] as const) {
    const color = inline(span, "color") ?? slot;
    assert.ok(color && contrast(color, background) >= 4.5, `${color} on ${background} reads at 4.5:1`);
  }
});

test("light: a prompt line on a dark bar has text that reads on the bar", async () => {
  const spans = await render("light", CLAUDE_DARK_THEME);
  const prompt = spans.get("prompt");
  assert.ok(prompt, "the prompt is drawn");
  const bar = inline(prompt, "background-color");
  const text = inline(prompt, "color");
  assert.equal(bar, "#373737", "the bar keeps the colour the CLI sent");
  assert.ok(text, `the text carries a corrected colour: ${prompt.getAttribute("style")}`);
  assert.ok(contrast(text, bar) >= 4.5, `${text} on ${bar} reads at 4.5:1, not a solid bar`);
});

test("light: inverse video stays readable under the floor", async () => {
  const spans = await render("light", "\x1b[7mselected\x1b[0m");
  const selected = spans.get("selected");
  assert.ok(selected, "the inverse cell is drawn");
  // Inverse of the defaults: the cell takes the foreground (xterm's slot 257)
  // and the text the background, unless the floor corrected it inline.
  assert.ok(selected.classList.contains("xterm-bg-257"), selected.outerHTML);
  const bar = TERMINAL_THEMES.light.foreground;
  const text = inline(selected, "color") ?? TERMINAL_THEMES.light.background;
  assert.ok(contrast(text, bar) >= 4.5, `${text} on ${bar} reads at 4.5:1`);
});

test("light: palette white and bright white clear 4.5:1 on the light background", () => {
  const light = TERMINAL_THEMES.light;
  for (const slot of ["foreground", "white", "brightWhite"] as const) {
    const color = light[slot];
    assert.ok(color, `${slot} is set`);
    assert.ok(contrast(color, light.background) >= 4.5, `${slot} ${color} on ${light.background}`);
  }
});

test("the contrast floor is Orca's: 4.5 on light, 3 on dark", () => {
  assert.equal(terminalAppearance("light").minimumContrastRatio, 4.5);
  assert.equal(terminalAppearance("dark").minimumContrastRatio, 3);
  assert.equal(terminalAppearance("light").theme, TERMINAL_THEMES.light);
  assert.equal(terminalAppearance("dark").theme, TERMINAL_THEMES.dark);
});
