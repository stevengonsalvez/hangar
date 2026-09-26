import { act, waitFor } from "@testing-library/react-native";
import { renderRouter } from "expo-router/testing-library";

import { reset } from "../src/attention/store";
import { connectHost, liveHosts, onAppState, resetLifecycle, retryAfterSecs } from "../src/lifecycle";
import { FakeWire, FAKE_HOST_A, FAKE_HOST_B } from "../src/wire/fake";
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

const hellos = async () => (await fake.connectionLog()).filter((l) => l.event === "hello").length;
const closes = async () => (await fake.connectionLog()).filter((l) => l.event === "close").length;

test("launch connects every paired host once, so a banner can arrive without opening a screen", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  expect(await screen.findByTestId("banner-att-1")).toBeTruthy(); // no host screen was opened
  expect(fake.isConnected(FAKE_HOST_A)).toBe(true);
  expect(fake.isConnected(FAKE_HOST_B)).toBe(false); // unreachable: the dial failed, nothing crashed
  await act(async () => {
    await connectHost(fake, FAKE_HOST_A); // idempotent: no second dial
  });
  expect(await hellos()).toBe(1);
  expect(liveHosts().get(FAKE_HOST_A)?.replay).toBe("complete"); // first connect asked for a snapshot
});

test("background closes the socket; foreground replays from the cursor and shows each new banner once", async () => {
  const screen = renderRouter("./app", { initialUrl: `/host/${FAKE_HOST_A}` });
  await screen.findByText("hangar");
  await screen.findByTestId("banner-att-1");
  expect(fake.isConnected(FAKE_HOST_A)).toBe(true);

  await act(() => onAppState(fake, "background"));
  expect(fake.isConnected(FAKE_HOST_A)).toBe(false);
  const before = liveHosts().get(FAKE_HOST_A)!.cursor!;

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
  expect(await closes()).toBe(2);
  expect(await hellos()).toBe(3);
});

test("an answer made elsewhere while backgrounded retires the row on foreground", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  await screen.findByTestId("banner-att-1");
  await act(() => onAppState(fake, "background"));
  fake.answeredElsewhere(FAKE_HOST_A, "att-1", "desktop@laptop");
  await act(() => onAppState(fake, "active"));
  expect(screen.queryByTestId("banner-att-1")).toBeNull();
});

test("iOS inactive changes nothing; only background closes", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  await screen.findByTestId("banner-att-1");
  await act(() => onAppState(fake, "inactive"));
  expect(fake.isConnected(FAKE_HOST_A)).toBe(true);
  expect(await closes()).toBe(0);
  await act(() => onAppState(fake, "background"));
  expect(fake.isConnected(FAKE_HOST_A)).toBe(false);
});

test("a background that begins while a foreground connect is in flight leaves no socket open", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  await screen.findByTestId("banner-att-1");
  await act(() => onAppState(fake, "background"));
  // Foreground and background back to back: the connect from the first must not outlive the second.
  const p1 = onAppState(fake, "active");
  const p2 = onAppState(fake, "background");
  await act(async () => {
    await Promise.all([p1, p2]);
  });
  expect(fake.isConnected(FAKE_HOST_A)).toBe(false);
  await waitFor(async () => expect(fake.isConnected(FAKE_HOST_A)).toBe(false));
});

test("a hook that throws or hangs does not stop the sockets from closing", async () => {
  const { beforeBackground } = require("../src/lifecycle") as typeof import("../src/lifecycle");
  const screen = renderRouter("./app", { initialUrl: "/" });
  await screen.findByTestId("banner-att-1");
  beforeBackground(() => {
    throw new Error("detach blew up");
  });
  beforeBackground(() => new Promise<void>(() => undefined)); // never settles
  void onAppState(fake, "background");
  await waitFor(() => expect(fake.isConnected(FAKE_HOST_A)).toBe(false), { timeout: 5000 });
});

test("a retryable close redials with backoff; a latching close does not", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  await screen.findByTestId("banner-att-1");
  await act(async () => fake.dropConnection(FAKE_HOST_A, 4503, "draining"));
  expect(fake.isConnected(FAKE_HOST_A)).toBe(false);
  await waitFor(() => expect(fake.isConnected(FAKE_HOST_A)).toBe(true), { timeout: 5000 }); // redialled after about 1 s

  await act(async () => fake.dropConnection(FAKE_HOST_A, 4403, "revoked"));
  await act(async () => {
    jest.advanceTimersByTime(70_000);
  });
  expect(fake.isConnected(FAKE_HOST_A)).toBe(false);
  expect(await hellos()).toBe(2);
});

test("retry-after is read from the close reason", () => {
  expect(retryAfterSecs("retry-after=7")).toBe(7);
  expect(retryAfterSecs("over capacity; retry-after=30")).toBe(30);
  expect(retryAfterSecs("draining")).toBeUndefined();
  expect(retryAfterSecs(undefined)).toBeUndefined();
  expect(retryAfterSecs("retry-after=-1")).toBeUndefined();
});

test("a failed redial is rescheduled with growing backoff until one succeeds", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  await screen.findByTestId("banner-att-1");
  fake.failNextConnect = 2; // the first two redials die on the wire
  await act(async () => fake.dropConnection(FAKE_HOST_A, undefined));
  await act(async () => {
    jest.advanceTimersByTime(1000); // attempt 1 at 1 s: fails
  });
  await act(async () => {
    jest.advanceTimersByTime(2000); // attempt 2 at +2 s: fails
  });
  expect(fake.isConnected(FAKE_HOST_A)).toBe(false);
  await act(async () => {
    jest.advanceTimersByTime(4000); // attempt 3 at +4 s: succeeds
  });
  await waitFor(() => expect(fake.isConnected(FAKE_HOST_A)).toBe(true), { timeout: 5000 });
  const log = await fake.connectionLog();
  expect(log.filter((l) => l.event === "dial-failed")).toHaveLength(2);
  expect(log.filter((l) => l.event === "hello")).toHaveLength(2);
});

test("retry-after is a floor under the backoff, on the first redial and every one after", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  await screen.findByTestId("banner-att-1");
  fake.failNextConnect = 1;
  await act(async () => fake.dropConnection(FAKE_HOST_A, 4429, "retry-after=5"));
  await act(async () => {
    jest.advanceTimersByTime(3000);
  });
  expect(fake.isConnected(FAKE_HOST_A)).toBe(false); // not yet: the 1 s backoff is under the 5 s floor
  await act(async () => {
    jest.advanceTimersByTime(2500); // 5 s: first redial, fails
  });
  expect(fake.isConnected(FAKE_HOST_A)).toBe(false);
  await act(async () => {
    jest.advanceTimersByTime(3000); // 2 s backoff would have fired here; the floor still holds
  });
  expect(fake.isConnected(FAKE_HOST_A)).toBe(false);
  await act(async () => {
    jest.advanceTimersByTime(2500); // second redial at +5 s
  });
  await waitFor(() => expect(fake.isConnected(FAKE_HOST_A)).toBe(true), { timeout: 5000 });
  expect((await fake.connectionLog()).filter((l) => l.event === "dial-failed")).toHaveLength(1);
});

test("a connect whose subscribe fails closes the socket at once and rethrows", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  await screen.findByTestId("banner-att-1");
  await act(() => onAppState(fake, "background"));
  fake.failNextSubscribe = 1;
  await expect(connectHost(fake, FAKE_HOST_A)).rejects.toThrow("subscribe failed");
  expect(fake.isConnected(FAKE_HOST_A)).toBe(false); // dialled, subscribe threw, closed before the caller heard
  expect(liveHosts().get(FAKE_HOST_A)?.connected).toBe(false);
  const log = await fake.connectionLog();
  expect(log.slice(-2).map((l) => l.event)).toEqual(["hello", "close"]);
});

test("a 4403 through either fake close hook latches the host", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  await screen.findByText("laptop");
  await act(async () => fake.dropConnection(FAKE_HOST_A, 4403, "revoked"));
  expect(await screen.findByText("revoked or expired, pair again")).toBeTruthy();
});

test("a resync request resubscribes from a snapshot", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  await screen.findByTestId("banner-att-1");
  fake.advance(FAKE_HOST_A, 5, true); // the phone missed these
  expect(liveHosts().get(FAKE_HOST_A)?.cursor).toBe(0);
  await act(async () => fake.resyncRequired(FAKE_HOST_A));
  await waitFor(() => expect(liveHosts().get(FAKE_HOST_A)?.cursor).toBe(5));
});
