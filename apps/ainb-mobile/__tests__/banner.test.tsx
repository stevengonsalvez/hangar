import { fireEvent, waitFor } from "@testing-library/react-native";
import { renderRouter } from "expo-router/testing-library";

import { reset } from "../src/attention/store";
import { resetLifecycle } from "../src/lifecycle";
import { FakeWire, FAKE_HOST_A } from "../src/wire/fake";
import { setWire } from "../src/wire";

let fake: FakeWire;
beforeEach(() => {
  reset();
  resetLifecycle();
  fake = new FakeWire();
  setWire(fake);
});

test("two taps send exactly one answer carrying the row's version as the fence", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  fireEvent.press(await screen.findByTestId("banner-att-1")); // tap 1
  fireEvent.press(await screen.findByTestId("option-2")); // tap 2
  expect(await screen.findByText("Delivered")).toBeTruthy();
  expect(fake.answers).toEqual([{ opId: "op-1", attentionId: "att-1", answer: "2", version: 3 }]);
  fireEvent.press(screen.getByTestId("answer-done"));
  await waitFor(() => expect(screen.queryByTestId("banner-att-1")).toBeNull());
});

test("a lost reply retries with the same op id and the ledger replays it: one delivery", async () => {
  fake.dropNextAnswer = true; // applied on the daemon, reply lost
  const screen = renderRouter("./app", { initialUrl: "/" });
  fireEvent.press(await screen.findByTestId("banner-att-1"));
  fireEvent.press(await screen.findByTestId("option-1"));
  expect(await screen.findByText(/No reply/)).toBeTruthy();
  expect(screen.queryByTestId("option-2")).toBeNull(); // the answer is pinned now
  fireEvent.press(screen.getByTestId("answer-retry"));
  expect(await screen.findByText("Delivered")).toBeTruthy();
  expect(fake.answers.map((a) => a.opId)).toEqual(["op-1", "op-1"]);
  expect(fake.answers.map((a) => a.answer)).toEqual(["1", "1"]);
  expect(fake.deliveries).toBe(1);
});

test("closing and reopening the sheet after a lost reply keeps the pinned answer and op id", async () => {
  fake.dropNextAnswer = true;
  const screen = renderRouter("./app", { initialUrl: "/" });
  fireEvent.press(await screen.findByTestId("banner-att-1"));
  fireEvent.press(await screen.findByTestId("option-2"));
  await screen.findByText(/No reply/);
  fireEvent.press(screen.getByTestId("answer-cancel"));
  await waitFor(() => expect(screen.queryByTestId("answer-sheet")).toBeNull());
  fireEvent.press(screen.getByTestId("banner-att-1")); // a new sheet instance
  expect(await screen.findByText("Sent: macos")).toBeTruthy();
  expect(screen.queryByTestId("option-1")).toBeNull();
  fireEvent.press(screen.getByTestId("answer-retry"));
  expect(await screen.findByText("Delivered")).toBeTruthy();
  expect(fake.answers.map((a) => `${a.opId}:${a.answer}`)).toEqual(["op-1:2", "op-1:2"]);
  expect(fake.deliveries).toBe(1);
});

test("a no_target outcome keeps the row open; only an answer retires it", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  fireEvent.press(await screen.findByTestId("banner-att-1"));
  fake.vanish(FAKE_HOST_A, "att-1");
  fireEvent.press(await screen.findByTestId("option-1"));
  expect(await screen.findByText("No live session to answer")).toBeTruthy();
  fireEvent.press(screen.getByTestId("answer-done"));
  expect(screen.getByTestId("banner-att-1")).toBeTruthy(); // still open until the daemon says otherwise
  // Nothing was taken, so the pin is gone: the options are back, and a different one may go out.
  fireEvent.press(screen.getByTestId("banner-att-1"));
  expect(await screen.findByTestId("option-2")).toBeTruthy();
});

test("already_answered_by names the winner", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  fireEvent.press(await screen.findByTestId("banner-att-1"));
  fake.answeredElsewhere(FAKE_HOST_A, "att-1", "desktop@laptop");
  fireEvent.press(await screen.findByTestId("option-1"));
  expect(await screen.findByText("Already answered by desktop@laptop")).toBeTruthy();
});

test("a raised ASK shows one banner and a free-text row answers with text", async () => {
  const screen = renderRouter("./app", { initialUrl: "/" });
  await screen.findByTestId("banner-att-1");
  await fake.connect(FAKE_HOST_A);
  fake.raise({ id: "att-2", sessionId: "s-2", sessionKey: "codex:web", kind: "codex_request_user", version: 1, createdAt: 9, payload: { text: "Which branch?" } });
  fake.raise({ id: "att-2", sessionId: "s-2", sessionKey: "codex:web", kind: "codex_request_user", version: 1, createdAt: 9, payload: { text: "Which branch?" } });
  expect(await screen.findByText("2 waiting · codex:web")).toBeTruthy();
  fireEvent.press(screen.getByTestId("banner-att-2"));
  fireEvent.changeText(await screen.findByTestId("answer-text"), "main");
  fireEvent.press(screen.getByTestId("answer-send"));
  expect(await screen.findByText("Delivered")).toBeTruthy();
  expect(fake.answers.at(-1)).toMatchObject({ attentionId: "att-2", answer: "main", version: 1 });
});
