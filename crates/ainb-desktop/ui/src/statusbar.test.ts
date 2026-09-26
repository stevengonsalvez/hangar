import { test } from "node:test";
import assert from "node:assert/strict";
import { daemonDotState } from "./statusbar.ts";

test("every SidecarState maps to exactly one dot", () => {
  assert.equal(daemonDotState({ state: "starting" }), "connecting");
  assert.equal(
    daemonDotState({ state: "connected", spawned: true, daemon_version: "1.0", protocol: { min: 1, max: 1 } }),
    "ok",
  );
  assert.equal(daemonDotState({ state: "reconnecting", error: "gone" }), "warn");
  assert.equal(
    daemonDotState({ state: "degraded", error: "no daemon", has_log: false }),
    "error",
  );
  assert.equal(
    daemonDotState({ state: "incompatible", message: "too old", daemon_is_newer: true }),
    "error",
  );
});
