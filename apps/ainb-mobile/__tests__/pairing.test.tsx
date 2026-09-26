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
  cam.scan = undefined; // never hit a previous test's camera instance
});

test("a scanned offer fills the field, shows the decoded host, and pairs only on the Pair tap", async () => {
  const screen = renderRouter("./app", { initialUrl: "/pair" });
  fireEvent.press(screen.getByTestId("scan"));
  await screen.findByTestId("camera");
  await waitFor(() => expect(cam.scan).toBeDefined());
  cam.scan!("not-an-offer");
  cam.scan!(`ainb://pair#${HOST_C}.k1`);
  cam.scan!(`ainb://pair#${HOST_C}.k9`); // a second frame changes nothing
  expect((await screen.findByTestId("offer")).props.value).toBe(`ainb://pair#${HOST_C}.k1`);
  expect(await screen.findByText(new RegExp(`host ${HOST_C} via lan`))).toBeTruthy();
  expect(screen).toHavePathname("/pair");
  expect((await fake.connectionLog()).some((l) => l.event === "paired")).toBe(false);
  fireEvent.press(screen.getByTestId("pair-submit"));
  await waitFor(() => expect(screen).toHavePathname("/"));
  expect(await screen.findByText("phone")).toBeTruthy();
  // paired, then connected at once (the lifecycle dials a new host without a screen visit)
  expect((await fake.connectionLog()).slice(-2).map((l) => `${l.hostId?.slice(-5)}:${l.event}`)).toEqual(["CCCCC:paired", "CCCCC:hello"]);
});

test("a deep link prefills the offer", async () => {
  jest.spyOn(deeplink, "useIncomingOffer").mockReturnValue(`ainb://pair#${HOST_C}.k1`);
  const screen = renderRouter("./app", { initialUrl: "/pair" });
  expect((await screen.findByTestId("offer")).props.value).toBe(`ainb://pair#${HOST_C}.k1`);
});

test("a second offer for the same host with a different key is refused as peer_changed", async () => {
  const screen = renderRouter("./app", { initialUrl: "/pair" });
  fireEvent.changeText(screen.getByTestId("offer"), `ainb://pair#${HOST_C}.k1`);
  await screen.findByTestId("offer-preview");
  fireEvent.press(screen.getByTestId("pair-submit"));
  await waitFor(() => expect(screen).toHavePathname("/"));
  fireEvent.press(screen.getByTestId("pair"));
  fireEvent.changeText(await screen.findByTestId("offer"), `ainb://pair#${HOST_C}.k2`);
  await screen.findByTestId("offer-preview");
  fireEvent.press(screen.getByTestId("pair-submit"));
  expect(await screen.findByText(/key changed/)).toBeTruthy();
  expect(screen).toHavePathname("/pair");
});

test("a 4403 close latches the host to re-pair; a 4503 does not", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  await screen.findByText("laptop");
  fake.closedBy(FAKE_HOST_A, 4503);
  await waitFor(() => expect((screen.getByText("reachable"))).toBeTruthy());
  expect(screen.queryByTestId(`repair-${FAKE_HOST_A}`)).toBeNull();

  fake.closedBy(FAKE_HOST_A, 4403);
  expect(await screen.findByText("revoked or expired, pair again")).toBeTruthy();
  fireEvent.press(screen.getByTestId(`host-${FAKE_HOST_A}`));
  expect(await screen.findByTestId("repair-notice")).toBeTruthy();
  expect(screen).toHavePathname("/pair");

  // A fresh offer clears the latch.
  fireEvent.changeText(screen.getByTestId("offer"), `ainb://pair#${FAKE_HOST_A}`);
  await screen.findByTestId("offer-preview");
  fireEvent.press(screen.getByTestId("pair-submit"));
  await waitFor(() => expect(screen).toHavePathname("/"));
  await waitFor(() => expect(screen.queryByTestId(`repair-${FAKE_HOST_A}`)).toBeNull());
});

test("a 4401 close latches identity re-pair", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  await screen.findByText("laptop");
  fake.closedBy(FAKE_HOST_A, 4401);
  expect(await screen.findByText("host no longer accepts this device, pair again")).toBeTruthy();
  fireEvent.press(screen.getByTestId(`host-${FAKE_HOST_A}`));
  expect(await screen.findByText(/no longer accepts this device. Pair again/)).toBeTruthy();
});

test("4409 and an unknown close code park the host row without a re-pair latch", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  await screen.findByText("server");
  fake.closedBy("01K5B0000000000000000BBBBB", 4409);
  expect(await screen.findByText("update the app or the host")).toBeTruthy();
  fake.closedBy("01K5B0000000000000000BBBBB", 4999);
  expect(await screen.findByText("closed with an unknown code, check the host")).toBeTruthy();
  expect(screen.queryByTestId("repair-01K5B0000000000000000BBBBB")).toBeNull();
});
