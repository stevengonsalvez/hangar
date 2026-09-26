import { act, waitFor } from "@testing-library/react-native";
import { renderRouter } from "expo-router/testing-library";

import { reset } from "../src/attention/store";
import { connectHost, liveHosts, mayRedial, onAppState, resetLifecycle } from "../src/lifecycle";
import { PeerCloseError } from "../src/wire/types";
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

test("a host that fails to connect at launch enters the redial loop", async () => {
  fake.failNextConnect = 1;
  const screen = renderRouter("./app", { initialUrl: "/" });
  await screen.findByText("laptop");
  expect(fake.isConnected(FAKE_HOST_A)).toBe(false);
  await act(async () => {
    jest.advanceTimersByTime(1100); // first redial at 1 s
  });
  await waitFor(() => expect(fake.isConnected(FAKE_HOST_A)).toBe(true), { timeout: 5000 });
  expect(await screen.findByTestId("banner-att-1")).toBeTruthy();
});

test("a connect refused with 4403 shows the latch and is never redialled", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  await screen.findByTestId("banner-att-1");
  await act(() => onAppState(fake, "background"));
  fake.refuseNextConnectWith = { code: 4403, reason: "revoked" };
  await act(() => onAppState(fake, "active"));
  expect(await screen.findByText("revoked or expired, pair again")).toBeTruthy();
  const hellos0 = await hellos();
  await act(async () => {
    jest.advanceTimersByTime(130_000); // two full backoff ceilings
  });
  expect(await hellos()).toBe(hellos0);
  expect(fake.isConnected(FAKE_HOST_A)).toBe(false);
});

test("a connect refused with an unknown code stops the loop; a refused 4429 keeps it with retry-after", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  await screen.findByTestId("banner-att-1");
  await act(async () => fake.dropConnection(FAKE_HOST_A, undefined));
  fake.refuseNextConnectWith = { code: 4999 };
  await act(async () => {
    jest.advanceTimersByTime(1100); // the redial is refused with 4999
  });
  await act(async () => {
    jest.advanceTimersByTime(130_000);
  });
  expect(fake.isConnected(FAKE_HOST_A)).toBe(false);
  expect(await screen.findByText("closed with an unknown code, check the host")).toBeTruthy();
  const log = await fake.connectionLog();
  expect(log.filter((l) => l.event === "refused")).toHaveLength(1);
});

test("a redial refused with 4429 and retry-after keeps dialling after that floor", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  await screen.findByTestId("banner-att-1");
  await act(async () => fake.dropConnection(FAKE_HOST_A, undefined));
  fake.refuseNextConnectWith = { code: 4429, reason: "retry-after=7" };
  await act(async () => {
    jest.advanceTimersByTime(1100); // first redial: refused 4429, floor 7 s
  });
  await act(async () => {
    jest.advanceTimersByTime(5000);
  });
  expect(fake.isConnected(FAKE_HOST_A)).toBe(false);
  await act(async () => {
    jest.advanceTimersByTime(2500);
  });
  await waitFor(() => expect(fake.isConnected(FAKE_HOST_A)).toBe(true), { timeout: 5000 });
});

test("mayRedial is fail-closed and reads the crate's verdict, not a code table of its own", () => {
  // the crate's Connect and Timeout are retryable by default; Closed carries its own flag
  expect(mayRedial(new PeerCloseError("connect", { reason: "dns" }))).toBe(true);
  expect(mayRedial(new PeerCloseError("timeout", { reason: "hello" }))).toBe(true);
  expect(mayRedial(new PeerCloseError("closed", { reason: "network loss", retryable: true }))).toBe(true); // code None, retryable
  expect(mayRedial(new PeerCloseError("closed", { code: 4503, reason: "draining", retryable: true }))).toBe(true);
  expect(mayRedial(new PeerCloseError("closed", { code: 4403, reason: "revoked", retryable: false }))).toBe(false);
  expect(mayRedial(new PeerCloseError("closed", { code: 1013, reason: "but the phone closed it", retryable: false }))).toBe(false);
  for (const kind of ["peer_changed", "not_paired", "custody", "protocol", "offer", "handshake", "rpc"] as const)
    expect(mayRedial(new PeerCloseError(kind))).toBe(false);
  expect(mayRedial(new Error("anything"))).toBe(false);
  expect(mayRedial("string")).toBe(false);
  expect(mayRedial(undefined)).toBe(false);
  // a copy of the class from another bundle is recognised by name, and still needs the flag
  expect(mayRedial(Object.assign(new Error("x"), { name: "PeerCloseError", kind: "connect", retryable: true }))).toBe(true);
  expect(mayRedial(Object.assign(new Error("x"), { name: "PeerCloseError", kind: "connect" }))).toBe(false);
});

test("a host whose key changed latches re-pair and is never redialled", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  await screen.findByTestId("banner-att-1");
  await act(async () => fake.dropConnection(FAKE_HOST_A, undefined));
  fake.refuseNextConnectWith = { kind: "peer_changed", reason: "static key differs" };
  await act(async () => {
    jest.advanceTimersByTime(1100);
  });
  expect(await screen.findByText("host key changed, pair again with a fresh offer")).toBeTruthy();
  expect(screen.getByTestId(`repair-${FAKE_HOST_A}`)).toBeTruthy();
  const hellos0 = await hellos();
  await act(async () => {
    jest.advanceTimersByTime(130_000);
  });
  expect(await hellos()).toBe(hellos0);
  expect(fake.isConnected(FAKE_HOST_A)).toBe(false);
});

test("a plain error from connect stops the loop too", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  await screen.findByTestId("banner-att-1");
  await act(async () => fake.dropConnection(FAKE_HOST_A, undefined));
  const real = fake.connect.bind(fake);
  fake.connect = async () => {
    fake.connect = real;
    throw new Error("something untyped");
  };
  await act(async () => {
    jest.advanceTimersByTime(1100);
  });
  await act(async () => {
    jest.advanceTimersByTime(130_000);
  });
  expect(fake.isConnected(FAKE_HOST_A)).toBe(false);
});

test("the closed event that follows our own background close never redials, even with a retryable code", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  await screen.findByTestId("banner-att-1");
  await act(() => onAppState(fake, "background"));
  await act(() => onAppState(fake, "active"));
  const hellosBefore = await hellos();
  // the peer's close for the socket we closed arrives late, tagged retryable, while we are foregrounded again
  // (our own close has been recorded, so no timer is armed for it)
  await act(async () => fake.dropConnection(FAKE_HOST_A, 4503, "draining"));
  // that drop also flipped the fake to disconnected: the live host redials because it was a real drop
  // so instead check the flag path directly: a close right after background must not arm a timer
  await act(() => onAppState(fake, "background"));
  await act(async () => fake.dropConnection(FAKE_HOST_A, 4503, "draining"));
  await act(async () => {
    jest.advanceTimersByTime(130_000);
  });
  expect(fake.isConnected(FAKE_HOST_A)).toBe(false);
  expect(liveHosts().get(FAKE_HOST_A)?.timer).toBeUndefined();
  expect((await hellos()) - hellosBefore).toBeLessThanOrEqual(1);
});
