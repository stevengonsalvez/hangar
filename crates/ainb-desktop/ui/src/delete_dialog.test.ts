import assert from "node:assert/strict";
import { test } from "node:test";
import type { Session_Serialize } from "../../../ainb-app/bindings/AppState";
import {
  changesLine,
  createDeleteFlow,
  deleteCopy,
  dirtyHint,
  forcesDelete,
  UNCHECKED_HINT,
  type DeletePreview,
  type PreviewState,
} from "./delete_dialog.ts";

const ready = (preview: DeletePreview): PreviewState => ({ kind: "ready", preview });

test("the dirty line is Orca's, singular and plural, and absent when clean", () => {
  assert.equal(dirtyHint(1), "1 uncommitted or untracked change");
  assert.equal(dirtyHint(3), "3 uncommitted or untracked changes");
  assert.equal(dirtyHint(0), null);
  assert.equal(dirtyHint(null), null);
});

test("a tree that goes with the session reads as Orca's worktree delete", () => {
  const copy = deleteCopy("Fix login", ready({ tree: "removed", changes: 0 }));
  assert.equal(copy.title, "Delete Workspace");
  assert.equal(`${copy.before}${copy.target}${copy.after}`, "Remove Fix login from git and delete its workspace folder.");
  assert.equal(copy.confirm, "Delete Workspace");
  assert.equal(copy.ready, true);
});

test("dirty or uncounted work asks for Force Delete, and says it forces", () => {
  for (const changes of [3, null]) {
    const copy = deleteCopy("Fix login", ready({ tree: "removed", changes }));
    assert.equal(copy.confirm, "Force Delete", `changes ${changes}`);
    assert.equal(copy.force, true);
    assert.equal(forcesDelete(ready({ tree: "removed", changes })), true);
  }
  assert.equal(deleteCopy("x", ready({ tree: "removed", changes: 0 })).force, false);
  // Nothing on disk goes with a shared or kept tree: nothing to force.
  assert.equal(forcesDelete(ready({ tree: "shared", changes: null })), false);
  assert.equal(forcesDelete(ready({ tree: "kept", changes: null })), false);
});

test("a forced confirm tells the host what it was shown", async () => {
  const sent: unknown[] = [];
  const flow = createDeleteFlow({
    preview: async () => ({ tree: "removed", changes: 3 }),
    remove: async (_id, confirmed) => void sent.push(confirmed),
    toast: () => undefined,
  });
  flow.open(pick("a"));
  await tick();
  flow.confirm();
  assert.deepEqual(sent, [{ expected: "removed", expectedChanges: 3, force: true }]);
});

test("a shared tree is never offered for removal: only the session goes", () => {
  const copy = deleteCopy("Fix login", ready({ tree: "shared", changes: null }));
  assert.equal(copy.title, "Delete Session");
  assert.equal(copy.confirm, "Delete Session");
  const said = `${copy.before}${copy.target}${copy.after}`;
  assert.match(said, /Another session still works in this worktree/);
  assert.match(said, /will not be deleted/);
  assert.doesNotMatch(said, /from git/);
});

test("a folder ainb did not make is kept, as Orca's folder workspace", () => {
  const copy = deleteCopy("mine", ready({ tree: "kept", changes: null }));
  assert.equal(`${copy.before}${copy.target}${copy.after}`, "Remove mine from ainb. The folder on disk will not be deleted.");
});

test("nothing is confirmable until the host has said what goes", () => {
  assert.equal(deleteCopy("x", { kind: "checking" }).ready, false);
  // Nor when it could not: the person would confirm blind.
  assert.equal(deleteCopy("x", { kind: "failed", reason: "no store" }).ready, false);
});

test("only a tree that goes counts its changes, and an unknown count says so", () => {
  assert.deepEqual(changesLine(ready({ tree: "removed", changes: 2 })), {
    text: "2 uncommitted or untracked changes",
    dirty: true,
  });
  assert.equal(changesLine(ready({ tree: "removed", changes: 0 })), null);
  assert.deepEqual(changesLine(ready({ tree: "removed", changes: null })), { text: UNCHECKED_HINT, dirty: false });
  assert.equal(changesLine(ready({ tree: "shared", changes: null })), null);
  assert.equal(changesLine({ kind: "checking" }), null);
});

const pick = (id: string) => ({
  action: "delete" as const,
  session: { id, workspace_path: `/wt/${id}` } as Session_Serialize,
  name: `name-${id}`,
});

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

test("confirm deletes once and closes; a second confirm does nothing", async () => {
  const removed: string[] = [];
  const flow = createDeleteFlow({
    preview: async () => ({ tree: "removed", changes: 0 }),
    remove: async (id, confirmed) => void removed.push(`${id}:${JSON.stringify(confirmed)}`),
    toast: () => undefined,
  });
  flow.open(pick("a"));
  flow.confirm();
  assert.deepEqual(removed, [], "not before the host has answered");
  await tick();
  flow.confirm();
  flow.confirm();
  // With the fate the person was shown, for the host to check again.
  assert.deepEqual(removed, ['a:{"expected":"removed","expectedChanges":0,"force":false}']);
  assert.equal(flow.target(), null);
});

test("a late preview for an earlier opening is dropped", async () => {
  const answers: ((preview: DeletePreview) => void)[] = [];
  const flow = createDeleteFlow({
    preview: () => new Promise((resolve) => answers.push(resolve)),
    remove: async () => undefined,
    toast: () => undefined,
  });
  flow.open(pick("a"));
  flow.close();
  flow.open(pick("b"));
  answers[0]({ tree: "shared", changes: null });
  await tick();
  assert.deepEqual(flow.state(), { kind: "checking" });
  answers[1]({ tree: "removed", changes: 4 });
  await tick();
  assert.deepEqual(flow.state(), ready({ tree: "removed", changes: 4 }));
});

test("a delete the host refuses is said in a toast", async () => {
  const toasts: string[] = [];
  const flow = createDeleteFlow({
    preview: async () => ({ tree: "removed", changes: 0 }),
    remove: () => Promise.reject("The session was not deleted: busy"),
    toast: (text) => toasts.push(text),
  });
  flow.open(pick("a"));
  await tick();
  flow.confirm();
  await tick();
  assert.deepEqual(toasts, ["The session was not deleted: busy"]);
});

test("a failed check cannot be confirmed", async () => {
  const removed: string[] = [];
  const flow = createDeleteFlow({
    preview: () => Promise.reject("no store"),
    remove: async (id) => void removed.push(id),
    toast: () => undefined,
  });
  flow.open(pick("a"));
  await tick();
  assert.deepEqual(flow.state(), { kind: "failed", reason: "no store" });
  flow.confirm();
  assert.deepEqual(removed, []);
});
