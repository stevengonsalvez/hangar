import { strict as assert } from "node:assert";
import { test } from "node:test";
import { NOTIFICATIONS_KEY, readNotifications, writeNotifications } from "./notifications.ts";
import type { ThemeStorage } from "./theme/theme.ts";

function memory(initial: Record<string, string> = {}): ThemeStorage & { data: Record<string, string> } {
  const data = { ...initial };
  return {
    data,
    getItem: (key) => (key in data ? data[key] : null),
    setItem: (key, value) => {
      data[key] = value;
    },
  };
}

const throwing: ThemeStorage = {
  getItem: () => {
    throw new Error("blocked");
  },
  setItem: () => {
    throw new Error("blocked");
  },
};

test("notifications are on until turned off", () => {
  assert.equal(readNotifications(undefined), true);
  assert.equal(readNotifications(memory()), true);
  assert.equal(readNotifications(memory({ [NOTIFICATIONS_KEY]: "neon" })), true);
  assert.equal(readNotifications(throwing), true);
  assert.equal(readNotifications(memory({ [NOTIFICATIONS_KEY]: "off" })), false);
});

test("a stored toggle round-trips, and a throwing storage does not break it", () => {
  const storage = memory();
  writeNotifications(storage, false);
  assert.equal(storage.data[NOTIFICATIONS_KEY], "off");
  assert.equal(readNotifications(storage), false);
  writeNotifications(storage, true);
  assert.equal(readNotifications(storage), true);
  assert.doesNotThrow(() => writeNotifications(throwing, false));
});
