import { act, fireEvent, waitFor } from "@testing-library/react-native";
import { renderRouter } from "expo-router/testing-library";
import * as webview from "react-native-webview";

import { reset } from "../src/attention/store";
import { onAppState, resetLifecycle } from "../src/lifecycle";
import { decodeInjection, encode } from "../src/terminal/engine/protocol";
import { FIXTURES } from "../src/terminal/fixtures";
import { FakeWire, FAKE_HOST_A } from "../src/wire/fake";
import { setWire } from "../src/wire";

const { bridge } = webview as unknown as typeof import("../__mocks__/react-native-webview");
const URL = `/host/${FAKE_HOST_A}/session/claude:hangar`;
/** What one fake attach paints: clear, the alt-screen snapshot, then the OSC 8 output line. */
const SNAPSHOT = ["clear", `write:${FIXTURES["f1-altscreen"].length}`, `write:${FIXTURES["f4-osc8"].length}`];

let fake: FakeWire;
beforeEach(() => {
  reset();
  resetLifecycle();
  bridge.reset();
  fake = new FakeWire();
  setWire(fake);
});

function sinkCalls() {
  return bridge.injected.map(decodeInjection).flatMap((m) => (m?.t === "write" ? [`write:${m.b64.length}`] : m?.t === "clear" ? ["clear"] : []));
}

async function openTerminal() {
  const screen = renderRouter("./app", { initialUrl: URL });
  await screen.findAllByText(/Which runner/); // transcript line and the banner
  fireEvent.press(screen.getByTestId("tab-terminal"));
  await screen.findByTestId("terminal");
  await act(async () => bridge.engineMessage!(encode({ t: "ready" })));
  return screen;
}

test("attach feeds the snapshot into the sink: clear, then the fixture bytes", async () => {
  const screen = await openTerminal();
  await waitFor(() => expect(sinkCalls()).toEqual(SNAPSHOT));
  expect(await screen.findByText("80x24")).toBeTruthy();
  expect(screen.getByTestId("native-badge")).toBeTruthy();
});

test("a mobile scope is read only: no toggle, no key bar, keys never reach the wire", async () => {
  const screen = await openTerminal();
  expect(screen.getByTestId("read-only")).toBeTruthy();
  expect(screen.queryByTestId("type-toggle")).toBeNull();
  expect(screen.queryByTestId("key-bar")).toBeNull();
  await act(async () => bridge.engineMessage!(encode({ t: "input", data: "x" })));
  expect(fake.inputs).toEqual([]);
});

const tick = () => act(async () => void jest.advanceTimersByTime(20));

test("mobile+type: the toggle acquires the floor and resizes to the phone's fit; keys batch per frame under the floor generation; a taken floor blocks keys locally; take-over wins it back", async () => {
  fake.info = { scope: { base: "mobile+type", admin: false }, capabilities: ["terminal.stream", "terminal.input"] };
  const screen = await openTerminal();
  await act(async () => bridge.engineMessage!(encode({ t: "fit", cols: 40, rows: 20 })));
  expect(await screen.findByText("80x24")).toBeTruthy(); // not the holder yet: no resize
  fireEvent.press(await screen.findByTestId("type-toggle"));
  await screen.findByText("typing");
  expect(await screen.findByText("40x20")).toBeTruthy(); // floor granted, geometry followed

  fireEvent.press(screen.getByTestId("key-esc"));
  fireEvent.press(screen.getByTestId("key-tab"));
  expect(fake.inputs).toEqual([]);
  await tick();
  await waitFor(() => expect(fake.inputs).toEqual([{ streamId: 1, floorGen: 1, data: "\x1b\t" }]));

  await act(async () => fake.floorTakenBy("claude:hangar", "desktop"));
  fireEvent.press(screen.getByTestId("key-tab"));
  await tick();
  expect(await screen.findByText("desktop has the floor")).toBeTruthy();
  expect(fake.inputs).toHaveLength(1); // nothing left the phone without the floor

  fireEvent.press(screen.getByTestId("take-over"));
  await waitFor(() => expect(screen.queryByTestId("floor-denied")).toBeNull());
  fireEvent.press(screen.getByTestId("key-tab"));
  await tick();
  await waitFor(() => expect(fake.inputs.at(-1)).toEqual({ streamId: 1, floorGen: 3, data: "\t" }));
});

test("a snapshot that lands before the engine is ready is painted once it is", async () => {
  const screen = renderRouter("./app", { initialUrl: URL });
  await screen.findAllByText(/Which runner/);
  fireEvent.press(screen.getByTestId("tab-terminal"));
  await screen.findByTestId("terminal");
  let frames = 0;
  const off = fake.onEvent((ev) => ev.kind === "terminal_frame" && frames++);
  await waitFor(() => expect(frames).toBeGreaterThanOrEqual(4)); // the snapshot has been sent
  expect(sinkCalls()).toEqual([]); // and queued: the engine has not said ready
  await act(async () => bridge.engineMessage!(encode({ t: "ready" })));
  expect(sinkCalls()).toEqual(SNAPSHOT);
  off();
});

test("background detaches and foreground re-attaches a fresh stream", async () => {
  const screen = await openTerminal();
  await waitFor(() => expect(sinkCalls()).toHaveLength(3));
  await act(() => onAppState(fake, "background"));
  expect(fake.detached).toEqual([1]);
  await act(() => onAppState(fake, "active"));
  await waitFor(() => expect(sinkCalls()).toHaveLength(6)); // a second clear + snapshot
  expect(await screen.findByText("80x24")).toBeTruthy();
});

test("a data gap clears the screen and the fresh snapshot follows", async () => {
  await openTerminal();
  await waitFor(() => expect(sinkCalls()).toHaveLength(3));
  await act(async () => fake.dropFeed(1));
  expect(sinkCalls()).toEqual([...SNAPSHOT, "clear", ...SNAPSHOT]);
});

test("the type toggle is hidden when the daemon does not advertise terminal.input", async () => {
  fake.info = { scope: { base: "mobile+type", admin: false }, capabilities: ["terminal.stream"] };
  const screen = await openTerminal();
  expect(screen.getByTestId("read-only")).toBeTruthy();
});

test("background detaches the stream before the socket closes", async () => {
  await openTerminal();
  await waitFor(() => expect(sinkCalls()).toHaveLength(3));
  await act(() => onAppState(fake, "background"));
  expect(fake.detached).toEqual([1]);
  const log = await fake.connectionLog();
  expect(log.at(-1)?.event).toBe("close");
});

test("the terminal tab shows the engine state and names an ignored link's host", async () => {
  const screen = await openTerminal();
  expect(await screen.findByText("engine ready")).toBeTruthy();
  await act(async () => bridge.engineMessage!(encode({ t: "link", uri: "https://example.invalid/never?x=1" })));
  expect(await screen.findByText("link ignored: example.invalid")).toBeTruthy();
});

test("frames that arrive before the attach reply is recorded are replayed, not lost", async () => {
  fake.snapshotBeforeReply = true; // the whole snapshot lands while the attach reply is still in flight
  const screen = await openTerminal();
  await waitFor(() => expect(sinkCalls()).toEqual(SNAPSHOT));
  await act(async () => bridge.engineMessage!(encode({ t: "stats", bytes: 2560, cols: 57, rows: 38 })));
  expect(await screen.findByText("engine ready, 2560 B, 57x38")).toBeTruthy();
});

test("an early-frame overflow discards the buffer and re-attaches once for a fresh snapshot", async () => {
  fake.snapshotBeforeReply = true;
  fake.floodBytesAfterSnapshot = 2 * 1024 * 1024 + 1024; // past the 2 MiB window, all before the attach reply lands
  const screen = await openTerminal();
  // both attaches overflow: one retry, then the terminal says why instead of spinning
  await waitFor(() => expect(fake.attaches).toBe(2), { timeout: 5000 });
  expect(fake.detached).toEqual([1, 2]);
  expect(await screen.findByText(/snapshot too large/)).toBeTruthy();
  expect(sinkCalls()).toEqual([]); // nothing from a discarded buffer was painted

  // a fresh cycle whose retry fits paints from its own snapshot
  fake.floodBytesAfterSnapshot = 0;
  await act(() => onAppState(fake, "background"));
  await act(() => onAppState(fake, "active"));
  await waitFor(() => expect(sinkCalls()).toEqual(SNAPSHOT), { timeout: 5000 });
  expect(await screen.findByText("80x24")).toBeTruthy();
});

test("an attach that throws leaves nothing buffered and the next attach starts clean", async () => {
  fake.failNextAttach = true;
  const screen = await openTerminal();
  expect(await screen.findByText("closed: attach refused")).toBeTruthy();
  expect(sinkCalls()).toEqual([]);
  await act(() => onAppState(fake, "background"));
  await act(() => onAppState(fake, "active")); // afterForeground re-attaches
  await waitFor(() => expect(sinkCalls()).toEqual(SNAPSHOT), { timeout: 5000 });
});

test("early frames of another stream never count against ours", async () => {
  fake.snapshotBeforeReply = true;
  const screen = await openTerminal();
  await waitFor(() => expect(sinkCalls()).toEqual(SNAPSHOT));
  // a stray stream's frames while a fresh attach is pending
  await act(() => onAppState(fake, "background"));
  const off = fake.onEvent(() => undefined);
  await act(async () => {
    fake.frame(99, { kind: "output", data: new Uint8Array(3 * 1024 * 1024) }); // unknown stream, huge
  });
  off();
  await act(() => onAppState(fake, "active"));
  await waitFor(() => expect(sinkCalls().slice(-3)).toEqual(SNAPSHOT), { timeout: 5000 });
  expect(screen.queryByText(/snapshot too large/)).toBeNull();
});
