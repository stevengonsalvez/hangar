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
  await waitFor(() => expect(sinkCalls()).toEqual(["clear", `write:${FIXTURES["f1-altscreen"].length}`]));
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
  await waitFor(() => expect(frames).toBeGreaterThanOrEqual(3)); // the snapshot has been sent
  expect(sinkCalls()).toEqual([]); // and queued: the engine has not said ready
  await act(async () => bridge.engineMessage!(encode({ t: "ready" })));
  expect(sinkCalls()).toEqual(["clear", `write:${FIXTURES["f1-altscreen"].length}`]);
  off();
});

test("background detaches and foreground re-attaches a fresh stream", async () => {
  const screen = await openTerminal();
  await waitFor(() => expect(sinkCalls()).toHaveLength(2));
  await act(() => onAppState(fake, "background"));
  expect(fake.detached).toEqual([1]);
  await act(() => onAppState(fake, "active"));
  await waitFor(() => expect(sinkCalls()).toHaveLength(4)); // a second clear + snapshot
  expect(await screen.findByText("80x24")).toBeTruthy();
});

test("a data gap clears the screen and the fresh snapshot follows", async () => {
  await openTerminal();
  await waitFor(() => expect(sinkCalls()).toHaveLength(2));
  await act(async () => fake.dropFeed(1));
  expect(sinkCalls()).toEqual(["clear", `write:${FIXTURES["f1-altscreen"].length}`, "clear", "clear", `write:${FIXTURES["f1-altscreen"].length}`]);
});

test("the type toggle is hidden when the daemon does not advertise terminal.input", async () => {
  fake.info = { scope: { base: "mobile+type", admin: false }, capabilities: ["terminal.stream"] };
  const screen = await openTerminal();
  expect(screen.getByTestId("read-only")).toBeTruthy();
});

test("background detaches the stream before the socket closes", async () => {
  await openTerminal();
  await waitFor(() => expect(sinkCalls()).toHaveLength(2));
  await act(() => onAppState(fake, "background"));
  expect(fake.detached).toEqual([1]);
  const log = await fake.connectionLog();
  expect(log.at(-1)?.event).toBe("close");
});
