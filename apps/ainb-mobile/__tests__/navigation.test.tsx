import { act, fireEvent, waitFor } from "@testing-library/react-native";
import { renderRouter } from "expo-router/testing-library";

import { resetLifecycle } from "../src/lifecycle";
import { FakeWire, FAKE_HOST_A } from "../src/wire/fake";
import { setWire } from "../src/wire";

beforeEach(() => {
  resetLifecycle();
  setWire(new FakeWire());
});

test("hosts, sessions, session and pair are reachable from the root", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  expect(await screen.findByText("laptop")).toBeTruthy();
  expect(screen.getByText(/unreachable since/)).toBeTruthy();

  fireEvent.press(screen.getByTestId(`host-${FAKE_HOST_A}`));
  expect(await screen.findByText("hangar")).toBeTruthy();
  expect(screen).toHavePathname(`/host/${FAKE_HOST_A}`);

  fireEvent.press(screen.getByTestId("session-claude:hangar"));
  expect(await screen.findByText(/Which runner do you mean/)).toBeTruthy();
  expect(screen).toHavePathname(`/host/${FAKE_HOST_A}/session/claude:hangar`);

  fireEvent.press(screen.getByTestId("tab-terminal"));
  expect(await screen.findByTestId("terminal")).toBeTruthy();
});

test("pairing a pasted offer adds the host", async () => {
  const screen = renderRouter("./app", { initialUrl: "/pair" });
  fireEvent.changeText(screen.getByTestId("offer"), "ainb://pair#01K5C0000000000000000CCCCC");
  fireEvent.changeText(screen.getByTestId("device-name"), "pixel");
  await screen.findByTestId("offer-preview");
  fireEvent.press(screen.getByTestId("pair-submit"));
  await waitFor(() => expect(screen).toHavePathname("/"));
  expect(await screen.findByText("pixel")).toBeTruthy();
});

test("send prompt with a stale fence shows turn_advanced", async () => {
  const fake = new FakeWire();
  setWire(fake);
  const screen = renderRouter("./app", { initialUrl: `/host/${FAKE_HOST_A}/session/claude:hangar` });
  await screen.findByText(/Which runner/);
  fake.advanceTurn(FAKE_HOST_A, "claude:hangar"); // the agent moved on under us
  fireEvent.changeText(screen.getByTestId("prompt"), "go");
  fireEvent.press(screen.getByTestId("send"));
  expect(await screen.findByText("Not sent: turn_advanced")).toBeTruthy();
});

test("interrupt sends the row version the user saw and an op id; a stale version is refused", async () => {
  const fake = new FakeWire();
  setWire(fake);
  const screen = renderRouter("./app", { initialUrl: `/host/${FAKE_HOST_A}/session/claude:hangar` });
  await screen.findAllByText(/Which runner/);
  fireEvent.press(screen.getByTestId("interrupt"));
  expect(await screen.findByText("Interrupted")).toBeTruthy();
  expect(fake.interrupts).toEqual([{ sessionKey: "claude:hangar", version: 1, opId: expect.stringMatching(/^op-\d+$/) }]);

  // the agent moved on under the user, silently: the row they saw is stale and the daemon says conflict (-32008)
  fake.advanceTurn(FAKE_HOST_A, "claude:hangar");
  fireEvent.press(screen.getByTestId("interrupt"));
  expect(await screen.findByText("Not interrupted: conflict")).toBeTruthy();
  expect(fake.interrupts).toHaveLength(2);
  expect(fake.interrupts[1]!.version).toBe(1); // the screen still holds the old row
  expect(fake.interrupts[1]!.opId).not.toBe(fake.interrupts[0]!.opId); // the accepted one was retired
});

test("the interrupt op id is re-minted when the row version changes, and reused while it does not", async () => {
  const fake = new FakeWire();
  setWire(fake);
  const screen = renderRouter("./app", { initialUrl: `/host/${FAKE_HOST_A}/session/claude:hangar` });
  await screen.findAllByText(/Which runner/);
  // make every interrupt a refusal so the op id is kept between taps
  await act(async () => fake.advanceTurn(FAKE_HOST_A, "claude:hangar", true));
  fireEvent.press(screen.getByTestId("interrupt"));
  await screen.findByText("Not interrupted: conflict");
  fireEvent.press(screen.getByTestId("interrupt"));
  await waitFor(() => expect(fake.interrupts).toHaveLength(2));
  expect(fake.interrupts[1]!.opId).toBe(fake.interrupts[0]!.opId); // same stale view, same op id
  expect(fake.interrupts[1]!.version).toBe(1);
  // the screen now learns the new row (version 2): a fresh op id goes out with it
  await act(async () => fake.advance(FAKE_HOST_A, 1));
  await waitFor(async () => expect((await fake.rosterStatus(FAKE_HOST_A))[0]!.version).toBe(2));
  fireEvent.press(screen.getByTestId("interrupt"));
  await waitFor(() => expect(fake.interrupts).toHaveLength(3));
  expect(fake.interrupts[2]!.version).toBe(2);
  expect(fake.interrupts[2]!.opId).not.toBe(fake.interrupts[0]!.opId);
  expect(await screen.findByText("Interrupted")).toBeTruthy();
});
