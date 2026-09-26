// The composer's field state: the daemon's own validation mirrored, the
// branch preview, the default project, and the blanks-to-null wire payload.

import assert from "node:assert/strict";
import { test } from "node:test";
import type { SessionsView_Serialize, Session_Serialize, Workspace_Serialize } from "../../../ainb-app/bindings/AppState";
import {
  branchPreview,
  BRANCH_PREFIX,
  defaultProjectPath,
  effectiveBranch,
  initialFields,
  projectChoices,
  sanitizeBranchSegment,
  SPAWN_FIELD_MAX,
  SPAWN_PROMPT_MAX,
  toArgs,
  validate,
  type ComposerFields,
} from "./composer.ts";

function session(id: string, over: Partial<Session_Serialize> = {}): Session_Serialize {
  return { id, name: id, status: "Running", branch_name: "main", ...over } as unknown as Session_Serialize;
}

function workspace(name: string, path: string, sessions: Session_Serialize[] = []): Workspace_Serialize {
  return { name, path, sessions, shell_session: null } as unknown as Workspace_Serialize;
}

function view(workspaces: Workspace_Serialize[], selectedSessionId: string | null = null): SessionsView_Serialize {
  return { workspaces, selected_session_id: selectedSessionId } as unknown as SessionsView_Serialize;
}

function fields(over: Partial<ComposerFields> = {}): ComposerFields {
  return {
    projectPath: "/repos/app",
    name: "",
    agent: "claude",
    model: "",
    prompt: "",
    branch: "",
    base: "",
    ...over,
  };
}

test("sanitizeBranchSegment lowercases, folds runs, and trims dashes", () => {
  assert.equal(sanitizeBranchSegment("Fix Login Bug"), "fix-login-bug");
  assert.equal(sanitizeBranchSegment("  --Weird!!Name--  "), "weird-name");
  assert.equal(sanitizeBranchSegment("already-fine"), "already-fine");
  assert.equal(sanitizeBranchSegment("---"), "");
  assert.equal(sanitizeBranchSegment(""), "");
});

test("branchPreview prefixes the sanitized segment, or is null for nothing to name", () => {
  assert.equal(branchPreview("Fix login bug"), `${BRANCH_PREFIX}/fix-login-bug`);
  assert.equal(branchPreview("   "), null);
  assert.equal(branchPreview("!!!"), null);
});

test("effectiveBranch prefers an explicit Advanced branch over the preview", () => {
  assert.equal(effectiveBranch(fields({ name: "fix login" })), `${BRANCH_PREFIX}/fix-login`);
  assert.equal(effectiveBranch(fields({ name: "fix login", branch: "feat/x" })), "feat/x");
  assert.equal(effectiveBranch(fields()), null, "no name and no branch: the daemon mints its own");
});

test("defaultProjectPath follows the selected session's workspace", () => {
  const workspaces = [workspace("api", "/repos/api", [session("a")]), workspace("web", "/repos/web", [session("b")])];
  assert.equal(defaultProjectPath(view(workspaces, "b")), "/repos/web");
  assert.equal(defaultProjectPath(view(workspaces, "a")), "/repos/api");
});

test("defaultProjectPath falls back to the first project, or empty with none", () => {
  const workspaces = [workspace("api", "/repos/api", [session("a")]), workspace("web", "/repos/web", [session("b")])];
  assert.equal(defaultProjectPath(view(workspaces, null)), "/repos/api");
  assert.equal(defaultProjectPath(view(workspaces, "does-not-exist")), "/repos/api");
  assert.equal(defaultProjectPath(view([], null)), "");
  assert.equal(defaultProjectPath(undefined), "");
});

test("initialFields opens on claude and the default project, every other field blank", () => {
  const workspaces = [workspace("api", "/repos/api", [session("a")])];
  const opened = initialFields(view(workspaces, "a"));
  assert.equal(opened.projectPath, "/repos/api");
  assert.equal(opened.agent, "claude");
  assert.equal(opened.name, "");
  assert.equal(opened.prompt, "");
});

test("projectChoices lists every workspace, in the frame's own order", () => {
  const workspaces = [workspace("api", "/repos/api"), workspace("web", "/repos/web")];
  assert.deepEqual(projectChoices(view(workspaces)), [
    { name: "api", path: "/repos/api" },
    { name: "web", path: "/repos/web" },
  ]);
  assert.deepEqual(projectChoices(undefined), []);
});

test("a plain request has no errors", () => {
  assert.deepEqual(validate(fields({ name: "fix login", prompt: "fix it" })), []);
});

test("a blank project is refused before an absolute-path check ever runs", () => {
  const errors = validate(fields({ projectPath: "" }));
  assert.deepEqual(errors, [{ field: "projectPath", message: "Choose a project." }]);
});

test("a relative project path is refused", () => {
  const errors = validate(fields({ projectPath: "repos/app" }));
  assert.equal(errors[0]?.field, "projectPath");
});

// The daemon's own refusal table (`ainb_hangar_proto::spawn` tests), mirrored:
// every one of these must be caught here, on the field the daemon would name.
const BAD_REFS = ["-rf", "--help", "a..b", "a b", "a~1", "x@{1}", "/a", "a/", "a.lock", ".a"];

test("refs that read as flags or bad refs are refused, on the Advanced branch field", () => {
  for (const bad of BAD_REFS) {
    const errors = validate(fields({ branch: bad }));
    assert.deepEqual(errors, [{ field: "branch", message: "Not a valid git ref." }], bad);
  }
});

test("refs that read as flags or bad refs are refused, on the base field", () => {
  for (const bad of BAD_REFS) {
    const errors = validate(fields({ base: bad }));
    assert.deepEqual(errors, [{ field: "base", message: "Not a valid git ref." }], bad);
  }
});

test("a name that sanitizes into an invalid branch is refused on the name field", () => {
  // Sanitizing keeps the dot, so this segment ends the ref in `.lock`.
  const errors = validate(fields({ name: "release.lock" }));
  assert.equal(errors[0]?.field, "name");
});

test("control characters and oversize fields are refused", () => {
  assert.equal(validate(fields({ model: "a\nb" }))[0]?.field, "model");
  assert.equal(validate(fields({ model: "m".repeat(SPAWN_FIELD_MAX + 1) }))[0]?.field, "model");
  assert.equal(validate(fields({ prompt: "x".repeat(SPAWN_PROMPT_MAX + 1) }))[0]?.field, "prompt");
  assert.equal(validate(fields({ prompt: "a\0b" }))[0]?.field, "prompt");
});

test("a blank optional field is never checked: it reaches the daemon as null, not as itself", () => {
  assert.deepEqual(validate(fields({ model: "", base: "", branch: "", prompt: "" })), []);
});

test("toArgs sends blank fields as null and trims the ones the daemon trims", () => {
  const args = toArgs(
    fields({
      projectPath: " /repos/app ",
      model: "  gpt  ",
      base: "  ",
      prompt: "",
    }),
  );
  assert.equal(args.repo_path, "/repos/app");
  assert.equal(args.model, "gpt");
  assert.equal(args.base, null);
  assert.equal(args.branch, null);
  assert.equal(args.prompt, null);
  assert.equal(args.name, null);
  assert.equal(args.agent, "claude");
});

test("toArgs sends the branch preview when Advanced's branch is blank", () => {
  const args = toArgs(fields({ name: "Fix Login Bug" }));
  assert.equal(args.branch, `${BRANCH_PREFIX}/fix-login-bug`);
});

test("toArgs sends an explicit Advanced branch over the preview", () => {
  const args = toArgs(fields({ name: "Fix Login Bug", branch: "feat/explicit" }));
  assert.equal(args.branch, "feat/explicit");
});

test("toArgs keeps the prompt exactly as typed, only blank becomes null", () => {
  const args = toArgs(fields({ prompt: "  fix the thing  " }));
  assert.equal(args.prompt, "  fix the thing  ");
  assert.equal(toArgs(fields({ prompt: "   " })).prompt, null);
});
