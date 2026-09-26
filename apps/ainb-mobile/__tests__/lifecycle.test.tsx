import { act } from "@testing-library/react-native";
import { renderRouter } from "expo-router/testing-library";

import { reset } from "../src/attention/store";
import { connectHost, liveHosts, onAppState, resetLifecycle } from "../src/lifecycle";
import { FakeWire, FAKE_HOST_A } from "../src/wire/fake";
import { setWire } from "../src/wire";

let fake: FakeWire;
beforeEach(() => {
  reset();
  resetLifecycle();
  fake = new FakeWire();
  setWire(fake);
});

function ask(id: string) {
  return { id, sessionId: "s-1", sessionKey: "claude:hangar", kind: "ask_user_question", version: 1, createdAt: 1, payload: { question: `q ${id}` } };
}

test("background closes the socket; foreground replays from the cursor and shows each new banner once", async () => {
  // The sessions screen connects the host itself (connectHost).
  const screen = renderRouter("./app", { initialUrl: `/host/${FAKE_HOST_A}` });
  await screen.findByText("hangar");
  await screen.findByTestId("banner-att-1");
  expect(fake.isConnected(FAKE_HOST_A)).toBe(true);

  await act(() => onAppState(fake, "background"));
  expect(fake.isConnected(FAKE_HOST_A)).toBe(false);
  const before = liveHosts().get(FAKE_HOST_A)!.cursor;

  // While we are away the daemon commits 20 fleet events and one new ASK.
  fake.advance(FAKE_HOST_A, 20);
  fake.raise(ask("att-2"));
  fake.raise(ask("att-2"));
  expect(screen.queryByTestId("banner-att-2")).toBeNull();

  await act(() => onAppState(fake, "active"));
  expect(fake.isConnected(FAKE_HOST_A)).toBe(true);
  const after = liveHosts().get(FAKE_HOST_A)!;
  expect(after.cursor).toBe(before + 21);
  expect(after.replay).toBe("complete");
  expect(await screen.findByText("2 waiting · claude:hangar")).toBeTruthy();
  expect(screen.getByTestId("banner-att-2")).toBeTruthy();

  // A second foreground does not re-show what was already raised.
  await act(() => onAppState(fake, "background"));
  await act(() => onAppState(fake, "active"));
  expect(screen.getByText("2 waiting · claude:hangar")).toBeTruthy();
  const log = await fake.connectionLog();
  expect(log.filter((l) => l.event === "close")).toHaveLength(2);
  expect(log.filter((l) => l.event === "hello")).toHaveLength(3);
});

test("an answer made elsewhere while backgrounded retires the row on foreground", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  await act(async () => {
    await connectHost(fake, FAKE_HOST_A);
  });
  await screen.findByTestId("banner-att-1");
  await act(() => onAppState(fake, "background"));
  fake.answeredElsewhere(FAKE_HOST_A, "att-1", "desktop@laptop");
  await act(() => onAppState(fake, "active"));
  expect(screen.queryByTestId("banner-att-1")).toBeNull();
});
