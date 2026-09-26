import { fireEvent, waitFor } from "@testing-library/react-native";
import { renderRouter } from "expo-router/testing-library";
import * as camera from "expo-camera";

import { reset } from "../src/attention/store";
import { resetLifecycle } from "../src/lifecycle";
import * as deeplink from "../src/pairing/deeplink";
import { FakeWire, FAKE_HOST_A } from "../src/wire/fake";
import { setWire } from "../src/wire";

const cam = (camera as unknown as typeof import("../__mocks__/expo-camera")).camera;
const HOST_C = "01K5C0000000000000000CCCCC";

let fake: FakeWire;
beforeEach(() => {
  reset();
  resetLifecycle();
  fake = new FakeWire();
  setWire(fake);
  jest.restoreAllMocks();
});

test("a scanned offer pairs without touching the text field", async () => {
  const screen = renderRouter("./app", { initialUrl: "/pair" });
  fireEvent.press(screen.getByTestId("scan"));
  await screen.findByTestId("camera");
  cam.scan!("not-an-offer");
  cam.scan!(`ainb://pair#${HOST_C}.k1`);
  await waitFor(() => expect(screen).toHavePathname("/"));
  expect(await screen.findByText("phone")).toBeTruthy();
  expect((await fake.connectionLog()).at(-1)).toMatchObject({ hostId: HOST_C, event: "paired" });
});

test("a deep link prefills the offer", async () => {
  jest.spyOn(deeplink, "useIncomingOffer").mockReturnValue(`ainb://pair#${HOST_C}.k1`);
  const screen = renderRouter("./app", { initialUrl: "/pair" });
  expect((await screen.findByTestId("offer")).props.value).toBe(`ainb://pair#${HOST_C}.k1`);
});

test("a second offer for the same host with a different key is refused as peer_changed", async () => {
  const screen = renderRouter("./app", { initialUrl: "/pair" });
  fireEvent.changeText(screen.getByTestId("offer"), `ainb://pair#${HOST_C}.k1`);
  fireEvent.press(screen.getByTestId("pair-submit"));
  await waitFor(() => expect(screen).toHavePathname("/"));
  fireEvent.press(screen.getByTestId("pair"));
  fireEvent.changeText(await screen.findByTestId("offer"), `ainb://pair#${HOST_C}.k2`);
  fireEvent.press(screen.getByTestId("pair-submit"));
  expect(await screen.findByText(/key changed/)).toBeTruthy();
  expect(screen).toHavePathname("/pair");
});

test("a 4403 close latches the host to re-pair; a 4503 does not", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  await screen.findByText("mbp");
  fake.closedBy(FAKE_HOST_A, 4503);
  await waitFor(() => expect((screen.getByText("reachable"))).toBeTruthy());
  expect(screen.queryByTestId(`repair-${FAKE_HOST_A}`)).toBeNull();

  fake.closedBy(FAKE_HOST_A, 4403);
  expect(await screen.findByText("revoked, pair again")).toBeTruthy();
  fireEvent.press(screen.getByTestId(`host-${FAKE_HOST_A}`));
  expect(await screen.findByTestId("repair-notice")).toBeTruthy();
  expect(screen).toHavePathname("/pair");

  // A fresh offer clears the latch.
  fireEvent.changeText(screen.getByTestId("offer"), `ainb://pair#${FAKE_HOST_A}`);
  fireEvent.press(screen.getByTestId("pair-submit"));
  await waitFor(() => expect(screen).toHavePathname("/"));
  await waitFor(() => expect(screen.queryByTestId(`repair-${FAKE_HOST_A}`)).toBeNull());
});

test("a 4401 close after expiry reads as expired", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  await screen.findByText("mbp");
  fake.closedBy(FAKE_HOST_A, 4401);
  expect(await screen.findByText("pairing expired, pair again")).toBeTruthy();
});
