import { fireEvent, waitFor } from "@testing-library/react-native";
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

  // the agent moved on under the user: the row they saw is stale
  fake.advanceTurn(FAKE_HOST_A, "claude:hangar");
  fireEvent.press(screen.getByTestId("interrupt"));
  expect(await screen.findByText("Not interrupted: turn_advanced")).toBeTruthy();
});
