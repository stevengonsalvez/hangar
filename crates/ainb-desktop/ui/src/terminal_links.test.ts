import assert from "node:assert/strict";
import { test } from "node:test";
import type { IBufferLine } from "@xterm/xterm";
import { hardWrappedUrl, linkClick, URL_MAX_LENGTH, URL_PATTERN, type Rows } from "./terminal_links.ts";

/** A buffer row as xterm keeps it: `你` takes two cells, its second empty. */
function line(text: string): IBufferLine {
  const cells: { chars: string; width: number }[] = [];
  for (const chars of text) {
    if (chars === "你") cells.push({ chars, width: 2 }, { chars: "", width: 0 });
    else cells.push({ chars, width: 1 });
  }
  return {
    isWrapped: false,
    length: cells.length,
    getCell: (x: number) => cells[x] && { getChars: () => cells[x].chars, getWidth: () => cells[x].width },
    translateToString: () => cells.map((cell) => cell.chars).join(""),
  } as unknown as IBufferLine;
}

const rows = (...texts: string[]): Rows => {
  const lines = texts.map(line);
  return { getLine: (y) => lines[y] };
};

/** A Claude Code panel: `│ ` on the left, the rule at column 49 on the right. */
const panel = (text: string) => `│ ${text.padEnd(46)} │`;

const OAUTH =
  "https://claude.ai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&response_type=code";
const [first, second, third] = [OAUTH.slice(0, 46), OAUTH.slice(46, 92), OAUTH.slice(92)];

test("a URL a panel wrapped over three rows links whole from any of them", () => {
  const buffer = rows(panel("Open this link to sign in:"), panel(first), panel(second), panel(third), panel(""));
  for (const y of [1, 2, 3]) {
    const found = hardWrappedUrl(buffer, y);
    assert.equal(found?.url, OAUTH, `row ${y}`);
    assert.deepEqual(found?.spans, [
      { y: 1, start: 2, end: 48 },
      { y: 2, start: 2, end: 48 },
      { y: 3, start: 2, end: 2 + third.length },
    ]);
  }
  assert.equal(hardWrappedUrl(buffer, 0), null, "the text above is not part of it");
  assert.equal(hardWrappedUrl(buffer, 4), null, "nor the blank row under it");
});

test("the full stop a sentence ends on is not the URL's", () => {
  const buffer = rows(panel(first), panel(second), panel(`${third}.`));
  const found = hardWrappedUrl(buffer, 2);
  assert.equal(found?.url, OAUTH);
  assert.equal(found?.spans.at(-1)?.end, 2 + third.length);
});

test("a short URL over one framed word is left to the addon, not joined to it", () => {
  const buffer = rows(panel("https://a.io/"), panel("docs"));
  assert.equal(hardWrappedUrl(buffer, 0), null);
  assert.equal(hardWrappedUrl(buffer, 1), null);
});

test("rows without a panel's rules are never scanned as a hard wrap", () => {
  const buffer = rows(first, second, third);
  assert.equal(hardWrappedUrl(buffer, 2), null);
});

test("a row whose right rule moved belongs to another panel", () => {
  const buffer = rows(panel(first), panel(second), `│ ${third.padEnd(30)} │`);
  // The first two rows alone are too little evidence of a wrap.
  assert.equal(hardWrappedUrl(buffer, 1), null);
  assert.equal(hardWrappedUrl(buffer, 2), null);
});

test("a second scheme starts the next URL, not this one's tail", () => {
  const buffer = rows(panel(first), panel(second), panel("https://example.com/next"));
  assert.equal(hardWrappedUrl(buffer, 2), null);
});

test("a framed URL past the length bound links nothing", () => {
  const filler = "a".repeat(46);
  const texts = [panel(`https://x.io/${filler.slice(13)}`)];
  while (texts.length * 46 <= URL_MAX_LENGTH) texts.push(panel(filler));
  const buffer = rows(...texts);
  assert.equal(hardWrappedUrl(buffer, texts.length - 1), null);
  assert.equal(hardWrappedUrl(buffer, 1), null);
  // The same panel one row under the bound links whole.
  const under = rows(...texts.slice(0, -1));
  assert.equal(hardWrappedUrl(under, 1)?.url.length, (texts.length - 1) * 46);
});

test("the scan up stops at the first row outside a panel", () => {
  const texts = [...Array.from({ length: 2000 }, () => "$ ls"), panel(first), panel(second), panel(third)];
  const lines = texts.map(line);
  let reads = 0;
  const buffer: Rows = {
    getLine: (y) => {
      reads += 1;
      return lines[y];
    },
  };
  assert.equal(hardWrappedUrl(buffer, texts.length - 1)?.url, OAUTH);
  assert.ok(reads < 20, `${reads} rows read for a three-row panel`);
});

test("wide characters are placed by cell, as Orca's panes place them", () => {
  const buffer = rows("|http://a/ab|", "|a你你你你你|", "|tailzzzzzzz|");
  const found = hardWrappedUrl(buffer, 1);
  assert.equal(found?.url, "http://a/aba你你你你你tailzzzzzzz");
  assert.deepEqual(found?.spans[1], { y: 1, start: 1, end: 12 });
});

test("the addon's pattern stops at a panel's rule", () => {
  assert.equal("│https://a.b/c│".match(URL_PATTERN)?.[0], "https://a.b/c");
  assert.equal("see (https://a.b/c?x=1).".match(URL_PATTERN)?.[0], "https://a.b/c?x=1");
  assert.equal("javascript:alert(1)".match(URL_PATTERN), null);
});

test("Cmd+click opens a link on macOS, Ctrl+click elsewhere; a bare click stays the pane's", () => {
  const click = (keys: Partial<MouseEvent> = {}) => ({
    button: 0,
    altKey: false,
    metaKey: false,
    ctrlKey: false,
    ...keys,
  });
  assert.ok(linkClick(click({ metaKey: true }), true));
  assert.ok(linkClick(click({ ctrlKey: true }), false));
  assert.ok(!linkClick(click(), true));
  assert.ok(!linkClick(click(), false));
  assert.ok(!linkClick(click({ ctrlKey: true }), true), "Ctrl+click is the pane's on macOS");
  assert.ok(!linkClick(click({ metaKey: true, altKey: true }), true), "Alt is the pane's");
  assert.ok(!linkClick(click({ ctrlKey: true, button: 2 }), false), "the right button is the menu's");
});
