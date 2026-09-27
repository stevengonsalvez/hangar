// Every case in the daemon's shared validation table through the composer's
// own `validate`. `crates/ainb-hangar-proto/tests/spawn_validation_parity.rs`
// runs the same file through `WorktreeCreateParams::validate`, so a rule
// changed on one side and not the other fails one of the two tests instead
// of reaching a person as a form that accepts what the daemon refuses.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { toArgs, validate, type ComposerFieldName, type ComposerFields, type CreateWorktreeArgs } from "./composer.ts";

interface Case {
  field: "branch" | "base" | "model" | "prompt" | "repo_path";
  value: string;
  ok: boolean;
  why: string;
}

const FIXTURE = new URL("../../../ainb-hangar-proto/tests/fixtures/spawn_validation.json", import.meta.url);
const cases: Case[] = JSON.parse(readFileSync(FIXTURE, "utf8")).cases;

/** A valid composer with only `c`'s field set, and the composer field whose
 * error that case must (or must not) raise. */
function fieldsFor(c: Case): { fields: ComposerFields; field: ComposerFieldName } {
  const fields: ComposerFields = {
    projectPath: "/repos/app",
    name: "",
    agent: "claude",
    model: "",
    prompt: "",
    branch: "",
    base: "",
  };
  switch (c.field) {
    case "branch":
      return { fields: { ...fields, branch: c.value }, field: "branch" };
    case "base":
      return { fields: { ...fields, base: c.value }, field: "base" };
    case "model":
      return { fields: { ...fields, model: c.value }, field: "model" };
    case "prompt":
      return { fields: { ...fields, prompt: c.value }, field: "prompt" };
    case "repo_path":
      return { fields: { ...fields, projectPath: c.value }, field: "projectPath" };
    default:
      throw new Error(`unknown field in fixture: ${String((c as { field: unknown }).field)}`);
  }
}

test("the shared table is the full one, not an empty file", () => {
  assert.ok(cases.length > 30, `only ${cases.length} cases`);
});

/** The wire key a blank optional field would reach the daemon under. */
const WIRE: Partial<Record<Case["field"], keyof CreateWorktreeArgs>> = {
  branch: "branch",
  base: "base",
  model: "model",
  prompt: "prompt",
};

/** A blank optional field reaches the daemon as `null` ("not given"), never
 * as the blank string the table refuses, so those cases are checked on the
 * wire payload instead of the validator. */
function sentAsNull(c: Case): boolean {
  return c.value.trim() === "" && WIRE[c.field] !== undefined;
}

test("a blank optional field is sent as not given, never as the blank the daemon refuses", () => {
  const blanks = cases.filter(sentAsNull);
  assert.ok(blanks.length > 0, "the table carries blank cases");
  for (const c of blanks) {
    const { fields } = fieldsFor(c);
    const key = WIRE[c.field] as keyof CreateWorktreeArgs;
    assert.equal(toArgs(fields)[key], null, `${c.field}=${JSON.stringify(c.value)} (${c.why})`);
  }
});

test("every shared case matches the composer's validator", () => {
  const wrong = cases.filter((c) => !sentAsNull(c)).flatMap((c) => {
    const { fields, field } = fieldsFor(c);
    const accepted = !validate(fields).some((error) => error.field === field);
    return accepted === c.ok ? [] : [`${c.field}=${JSON.stringify(c.value.slice(0, 40))} expected ok=${c.ok} (${c.why})`];
  });
  assert.deepEqual(wrong, [], `composer validator disagrees with the shared cases:\n${wrong.join("\n")}`);
});
