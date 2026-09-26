// Done-until-ack, per viewer: the pure map and its storage, wrapped the way
// `theme.ts`'s own preference is.

import assert from "node:assert/strict";
import { test } from "node:test";
import { ackTurn, isAcked, NO_ACKS, readAcks, writeAcks, type AckStorage } from "./acks.ts";

function fakeStorage(initial: Record<string, string> = {}): AckStorage {
  const data = { ...initial };
  return {
    getItem: (key) => data[key] ?? null,
    setItem: (key, value) => {
      data[key] = value;
    },
  };
}

function throwingStorage(): AckStorage {
  return {
    getItem() {
      throw new Error("blocked site data");
    },
    setItem() {
      throw new Error("blocked site data");
    },
  };
}

test("isAcked is false for a session with no ack at all", () => {
  assert.equal(isAcked(NO_ACKS, "claude:p-1", 5), false);
});

test("ackTurn acks exactly the turn it was given, not every turn before it", () => {
  const acks = ackTurn(NO_ACKS, "claude:p-1", 5);
  assert.equal(isAcked(acks, "claude:p-1", 5), true, "this turn is acked");
  assert.equal(isAcked(acks, "claude:p-1", 6), false, "a later turn is not");
  assert.equal(isAcked(acks, "claude:p-2", 5), false, "another session is untouched");
});

test("acking never moves an ack backwards", () => {
  const newer = ackTurn(NO_ACKS, "claude:p-1", 9);
  const older = ackTurn(newer, "claude:p-1", 5);
  assert.equal(older, newer, "acking an older marker after a newer one is a no-op, same object");
  assert.equal(isAcked(older, "claude:p-1", 9), true, "the newer ack still holds");
});

test("acking the same turn twice returns the same map rather than a new one", () => {
  const once = ackTurn(NO_ACKS, "claude:p-1", 5);
  const twice = ackTurn(once, "claude:p-1", 5);
  assert.equal(twice, once);
});

test("readAcks round-trips through writeAcks", () => {
  const storage = fakeStorage();
  const acks = ackTurn(ackTurn(NO_ACKS, "claude:p-1", 5), "codex:p-2", 3);
  writeAcks(storage, acks);
  assert.deepEqual(readAcks(storage), acks);
});

test("readAcks is empty for absent, malformed, or foreign storage", () => {
  assert.deepEqual(readAcks(undefined), NO_ACKS);
  assert.deepEqual(readAcks(fakeStorage({ "ainb.board.acks": "not json" })), NO_ACKS);
  assert.deepEqual(readAcks(fakeStorage({ "ainb.board.acks": "[1,2,3]" })), NO_ACKS, "an array, not the map shape");
  assert.deepEqual(
    readAcks(fakeStorage({ "ainb.board.acks": JSON.stringify({ "claude:p-1": "five" }) })),
    NO_ACKS,
    "a value that is not a number is dropped",
  );
});

test("a storage that throws loses the memory of an ack for this window's life, and never crashes", () => {
  const storage = throwingStorage();
  assert.deepEqual(readAcks(storage), NO_ACKS);
  assert.doesNotThrow(() => writeAcks(storage, ackTurn(NO_ACKS, "claude:p-1", 5)));
  assert.equal(readAcks(undefined), NO_ACKS);
});
