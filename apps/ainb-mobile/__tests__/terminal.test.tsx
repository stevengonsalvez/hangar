import { act, fireEvent, render } from "@testing-library/react-native";
import * as webview from "react-native-webview";

import { concat, makeCoalescer } from "../src/terminal/engine/coalesce";
import { decodeFromEngine, decodeInjection, encode, fromBase64, toBase64 } from "../src/terminal/engine/protocol";
import { FIXTURES } from "../src/terminal/fixtures";
import { withCtrl } from "../src/terminal/keys";
import { ENGINE_URL, TerminalView, type TerminalSink } from "../src/terminal/TerminalView";

// The native webview is the shared jest mock (__mocks__/react-native-webview.tsx, jest.setup.ts).
const { bridge } = webview as unknown as typeof import("../__mocks__/react-native-webview");

beforeEach(() => bridge.reset());

function injectedWrites(): string[] {
  return bridge.injected.map(decodeInjection).flatMap((m) => (m?.t === "write" ? [m.b64] : []));
}

test("the coalescer flushes one batch per tick and concat joins the chunks", () => {
  const ticks: (() => void)[] = [];
  const flushed: Uint8Array[][] = [];
  const c = makeCoalescer<Uint8Array>((b) => flushed.push(b), (cb) => ticks.push(cb));
  c.push(new Uint8Array([1]));
  c.push(new Uint8Array([2, 3]));
  c.push(new Uint8Array([4]));
  expect(ticks).toHaveLength(1);
  expect(flushed).toHaveLength(0);
  ticks[0]!();
  expect(flushed).toHaveLength(1);
  expect([...concat(flushed[0]!)]).toEqual([1, 2, 3, 4]);
  c.push(new Uint8Array([5]));
  expect(ticks).toHaveLength(2);
});

test("bridge messages round-trip and unknown ones are dropped", () => {
  expect(decodeFromEngine(encode({ t: "fit", cols: 40, rows: 20 }))).toEqual({ t: "fit", cols: 40, rows: 20 });
  expect(decodeFromEngine(encode({ t: "input", data: "\x1b[A" }))).toEqual({ t: "input", data: "\x1b[A" });
  expect(decodeFromEngine('{"t":"bell"}')).toBeUndefined();
  expect(decodeFromEngine("nope")).toBeUndefined();
  const bytes = fromBase64(FIXTURES["f3-widechars"]);
  expect(bytes).toHaveLength(276);
  expect(toBase64(bytes)).toBe(FIXTURES["f3-widechars"]);
  expect(fromBase64(FIXTURES["f1-altscreen"])).toHaveLength(2284);
  expect(new TextDecoder().decode(fromBase64(FIXTURES["f4-osc8"]))).toContain("\x1b]8;;https://example.invalid/never\x1b\\");
});

test("ctrl turns a letter into its control byte", () => {
  expect(withCtrl("c")).toBe("\x03");
  expect(withCtrl("C")).toBe("\x03");
  expect(withCtrl("[")).toBe("\x1b");
  expect(withCtrl("\x1b[A")).toBe("\x1b[A");
});

test("the sink exists from mount, bytes written before ready are injected once the engine is up, and keys reach onInput", async () => {
  const input: string[] = [];
  let sink: TerminalSink | undefined;
  const screen = render(<TerminalView onSink={(s) => (sink = s)} onInput={(d) => input.push(d)} onFit={() => undefined} />);
  expect(sink).toBeDefined();
  sink!.write(new Uint8Array([104, 105]));
  expect(injectedWrites()).toEqual([]);
  await act(async () => bridge.engineMessage!(encode({ t: "ready" })));
  expect(injectedWrites()).toEqual([toBase64(new Uint8Array([104, 105]))]);

  fireEvent.press(screen.getByTestId("key-ctrl"));
  await act(async () => bridge.engineMessage!(encode({ t: "input", data: "c" })));
  fireEvent.press(screen.getByTestId("key-esc"));
  fireEvent.press(screen.getByTestId("key-↑"));
  expect(input).toEqual(["\x03", "\x1b", "\x1b[A"]);
});

test("a read-only terminal has no key bar", () => {
  const screen = render(<TerminalView onSink={() => undefined} />);
  expect(screen.queryByTestId("key-bar")).toBeNull();
  expect(screen.getByTestId("terminal-webview")).toBeTruthy();
});

test("the webview is locked to the inline document and ignores a foreign page's messages", async () => {
  const input: string[] = [];
  render(<TerminalView onSink={() => undefined} onInput={(d) => input.push(d)} />);
  const p = bridge.props!;
  expect(p.originWhitelist).toEqual([ENGINE_URL]);
  expect((p.onShouldStartLoadWithRequest as (r: { url: string }) => boolean)({ url: "https://evil.example/" })).toBe(false);
  expect((p.onShouldStartLoadWithRequest as (r: { url: string }) => boolean)({ url: ENGINE_URL })).toBe(true);
  expect(p.setSupportMultipleWindows).toBe(false);
  expect(p.javaScriptCanOpenWindowsAutomatically).toBe(false);
  expect(p.allowFileAccess).toBe(false);
  expect(p.mixedContentMode).toBe("never");
  expect(p.incognito).toBe(true);

  await act(async () => bridge.engineMessage!(encode({ t: "ready" }), "https://evil.example/"));
  await act(async () => bridge.engineMessage!(encode({ t: "input", data: "rm -rf\r" }), "https://evil.example/"));
  expect(input).toEqual([]);
  await act(async () => bridge.engineMessage!(encode({ t: "ready" })));
  await act(async () => bridge.engineMessage!(encode({ t: "input", data: "ok" })));
  expect(input).toEqual(["ok"]);
});

test("the engine document carries a no-network CSP and disables link activation", () => {
  const { TERMINAL_HTML } = require("../src/terminal/engine/bundle.generated") as { TERMINAL_HTML: string };
  expect(TERMINAL_HTML).toContain('http-equiv="Content-Security-Policy"');
  expect(TERMINAL_HTML).toContain("default-src 'none'");
  expect(TERMINAL_HTML).toContain("frame-src 'none'");
  expect(TERMINAL_HTML).toMatch(/linkHandler:\{activate:/);
});

test("a link activation reaches the host as a report, and the engine state is reported", async () => {
  const links: string[] = [];
  const states: string[] = [];
  render(<TerminalView onSink={() => undefined} onLink={(u) => links.push(u)} onEngine={(st) => states.push(st)} />);
  await act(async () => bridge.engineMessage!(encode({ t: "link", uri: "https://example.invalid/never" }), "https://evil.example/"));
  expect(links).toEqual([]);
  expect(states).toEqual(["dropped message from https://evil.example/"]);
  await act(async () => bridge.engineMessage!(encode({ t: "ready" })));
  await act(async () => bridge.engineMessage!(encode({ t: "link", uri: "https://example.invalid/never" })));
  expect(links).toEqual(["https://example.invalid/never"]);
  expect(states).toEqual(["dropped message from https://evil.example/", "engine ready"]);
  expect(decodeFromEngine(encode({ t: "link", uri: "x".repeat(2000) }))).toEqual({ t: "link", uri: "x".repeat(512) });
});
