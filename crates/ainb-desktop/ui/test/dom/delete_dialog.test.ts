// The row menu's Delete, mounted as `main.tsx` mounts it: the sidebar's menu
// hands the pick to a delete flow, the flow opens the confirmation, and only
// the dialog's confirm reaches the host's delete. `src/delete_dialog.test.ts`
// holds the copy and the flow's rules; this proves the window draws and wires
// them: the dirty line, the shared-tree case, the keyboard and the focus.

import { hostCalls, hostReplies, settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent, Show } from "solid-js";
import { render } from "solid-js/web";
import type { Session_Serialize, SessionsView_Serialize } from "../../../../ainb-app/bindings/AppState";
import { createDeleteFlow, type Confirmed, type DeleteFlowDeps, type DeletePreview } from "../../src/delete_dialog.ts";
import { DeleteDialog } from "../../src/delete_dialog.tsx";
import { runRowPick } from "../../src/row_menu.ts";
import { Sidebar } from "../../src/sidebar.tsx";

function session(id: string, workspacePath: string, over: Partial<Session_Serialize> = {}): Session_Serialize {
  return {
    id,
    name: id,
    workspace_path: workspacePath,
    branch_name: `ainb/${id}`,
    container_id: null,
    status: "Running",
    created_at: "2024-01-01T00:00:00Z",
    last_accessed: "2024-01-01T00:00:00Z",
    git_changes: { added: 0, modified: 0, deleted: 0 },
    recent_logs: null,
    skip_permissions: false,
    mode: "Interactive",
    boss_prompt: null,
    agent_type: "Claude",
    model: null,
    codex_model: null,
    ssh_target: null,
    display_name: null,
    tmux_session_name: null,
    preview_content: null,
    is_attached: false,
    attention: [],
    ...over,
  } as Session_Serialize;
}

const SESSIONS = [
  session("claude-1", "/repo/wt-a", { display_name: "Fix login" }),
  session("shell-1", "/repo/wt-a", { agent_type: "Shell", created_at: "2023-01-01T00:00:00Z" }),
];

function frame(sessions: Session_Serialize[]): SessionsView_Serialize {
  return {
    workspaces: [{ name: "repo", path: "/repo", sessions, shell_session: null }],
    selected_workspace_index: 0,
    selected_session_id: null,
    shell_selected: false,
    selected_sessions: [],
    expand_all_workspaces: false,
    session_filter: "active_only",
    attached_session_id: null,
    favorite_workspace_paths: [],
  } as SessionsView_Serialize;
}

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  document.body.innerHTML = "";
  hostCalls.clear();
  hostReplies.clear();
});

/** What the host was asked, in order. */
type Asked = ["preview", string] | ["delete", string, Confirmed];

/** Mount the sidebar and the delete dialog over one flow, as `main.tsx`
 * does. `preview` is the host's answer; the default flow's host commands are
 * used when `deps` is `"host"`. */
async function mount(preview: DeletePreview | Error, deps: "fake" | "host" = "fake") {
  const asked: Asked[] = [];
  const toasts: string[] = [];
  const fake: DeleteFlowDeps = {
    preview: (id) => {
      asked.push(["preview", id]);
      return preview instanceof Error ? Promise.reject(preview.message) : Promise.resolve(preview);
    },
    remove: async (id, confirmed) => void asked.push(["delete", id, confirmed]),
    toast: (text) => toasts.push(text),
  };
  const flow = createDeleteFlow(deps === "fake" ? fake : { toast: fake.toast });
  const container = document.createElement("div");
  document.body.appendChild(container);
  cleanup = render(
    () => [
      createComponent(Sidebar, {
        sessions: frame(SESSIONS),
        stale: false,
        loading: false,
        pending: null,
        onOpen() {},
        onNew() {},
        ref() {},
        onRowPick: (pick) =>
          runRowPick(pick, {
            open() {},
            run() {},
            copy() {},
            reselect: () => [],
            confirmDelete: flow.open,
          }),
      }),
      createComponent(Show, {
        get when() {
          return flow.target();
        },
        children: (target: () => NonNullable<ReturnType<typeof flow.target>>) =>
          createComponent(DeleteDialog, {
            get target() {
              return target();
            },
            get state() {
              return flow.state();
            },
            onConfirm: flow.confirm,
            onClose: flow.close,
          }),
      }),
    ],
    container,
  );
  await settle();
  return { asked, toasts };
}

const row = (id: string) => document.querySelector<HTMLButtonElement>(`.session-row[data-session="${id}"]`)!;
const dialog = () => document.querySelector<HTMLElement>('[role="alertdialog"]');
const button = (text: string) =>
  [...document.querySelectorAll<HTMLButtonElement>(".delete-dialog button")].find((el) => el.textContent === text)!;

function focusIs(expected: Element | undefined, message = "focus") {
  const active = document.activeElement;
  assert.ok(active === expected, `${message}: focus is on ${active?.outerHTML.slice(0, 80) ?? "nothing"}`);
}

/** Right-click `id`'s row and choose Delete, as a person does. */
async function chooseDelete(id: string) {
  const target = row(id);
  target.dispatchEvent(
    new window.MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 40, clientY: 60 }) as unknown as Event,
  );
  await settle();
  const item = [...document.querySelectorAll<HTMLButtonElement>('[role="menuitem"]')].find(
    (el) => el.textContent === "Delete",
  )!;
  item.click();
  await settle();
}

async function key(target: Element, init: KeyboardEventInit) {
  const event = new window.KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init });
  target.dispatchEvent(event as unknown as Event);
  await settle();
  return event;
}

test("the menu's Delete opens the confirmation and deletes nothing", async () => {
  const { asked } = await mount({ tree: "removed", changes: 0 });
  await chooseDelete("claude-1");
  const open = dialog();
  assert.ok(open, "a confirmation is drawn");
  assert.equal(open.getAttribute("aria-modal"), "true");
  assert.equal(open.querySelector("h2")?.textContent, "Delete Workspace");
  assert.equal(
    open.querySelector("#delete-dialog-description")?.textContent,
    "Remove Fix login from git and delete its workspace folder.",
  );
  assert.equal(open.querySelector(".delete-dialog-path")?.textContent, "/repo/wt-a");
  assert.deepEqual(asked, [["preview", "claude-1"]], "asked what goes, deleted nothing");
  focusIs(button("Delete Workspace"), "Orca's confirm takes the keyboard");
});

test("a dirty worktree shows Orca's hint and its tooltip", async () => {
  await mount({ tree: "removed", changes: 3 });
  await chooseDelete("claude-1");
  const hint = document.querySelector<HTMLElement>(".delete-dialog-dirty");
  assert.ok(hint, "the dirty line is drawn");
  assert.equal(hint.textContent, "3 uncommitted or untracked changes");
  assert.equal(hint.title, "Deleting this workspace permanently removes these changes from disk.");
});

test("a dirty worktree asks for Force Delete, with the keyboard on Cancel", async () => {
  // Orca's `canForceDelete` footer (`DeleteWorktreeDialogFooter.tsx:39-40`):
  // Enter on the opened dialog must not wipe the work it just listed.
  await mount({ tree: "removed", changes: 3 });
  await chooseDelete("claude-1");
  assert.ok(button("Force Delete"), "the confirm says what it does");
  focusIs(button("Cancel"), "Cancel has the keyboard");
});

test("an uncounted worktree asks for Force Delete too", async () => {
  await mount({ tree: "removed", changes: null });
  await chooseDelete("claude-1");
  assert.equal(document.querySelector(".delete-dialog-unchecked")?.textContent, "Could not check this worktree for uncommitted changes.");
  assert.ok(button("Force Delete"));
  focusIs(button("Cancel"));
});

test("a clean worktree keeps Orca's Delete, with the keyboard on it", async () => {
  await mount({ tree: "removed", changes: 0 });
  await chooseDelete("claude-1");
  assert.ok(button("Force Delete") === undefined);
  focusIs(button("Delete Workspace"));
});

test("a clean worktree shows no dirty line", async () => {
  await mount({ tree: "removed", changes: 0 });
  await chooseDelete("claude-1");
  assert.ok(document.querySelector(".delete-dialog-dirty") === null);
});

test("confirm calls the host's delete once for the row, and closes", async () => {
  const { asked } = await mount({ tree: "removed", changes: 1 });
  await chooseDelete("shell-1");
  button("Force Delete").click();
  await settle();
  assert.ok(dialog() === null, "the dialog gets out of the way as the delete starts");
  assert.deepEqual(asked, [
    ["preview", "shell-1"],
    // What the person was shown, for the host to count again.
    ["delete", "shell-1", { expected: "removed", expectedChanges: 1, force: true }],
  ]);
});

test("pressing the focused confirm twice deletes once", async () => {
  const { asked } = await mount({ tree: "removed", changes: 0 });
  await chooseDelete("claude-1");
  // What Delete then Enter presses: the confirm has the keyboard. A second
  // press lands after the dialog closed, on whatever has focus then.
  (document.activeElement as HTMLButtonElement).click();
  (document.activeElement as HTMLButtonElement | null)?.click?.();
  await settle();
  assert.deepEqual(
    asked.filter(([kind]) => kind === "delete"),
    [["delete", "claude-1", { expected: "removed", expectedChanges: 0, force: false }]],
  );
});

test("Cancel closes without deleting and gives the keyboard back to the row", async () => {
  const { asked } = await mount({ tree: "removed", changes: 0 });
  await chooseDelete("claude-1");
  button("Cancel").click();
  await settle();
  assert.ok(dialog() === null);
  assert.deepEqual(asked, [["preview", "claude-1"]]);
  focusIs(row("claude-1"), "the row that opened it");
});

test("Esc closes without deleting and gives the keyboard back to the row", async () => {
  const { asked } = await mount({ tree: "removed", changes: 2 });
  await chooseDelete("claude-1");
  const pressed = await key(document.activeElement!, { key: "Escape" });
  assert.equal(pressed.defaultPrevented, true);
  assert.ok(dialog() === null);
  assert.deepEqual(asked, [["preview", "claude-1"]]);
  focusIs(row("claude-1"));
});

test("Tab stays inside the dialog", async () => {
  await mount({ tree: "removed", changes: 0 });
  await chooseDelete("claude-1");
  focusIs(button("Delete Workspace"));
  await key(document.activeElement!, { key: "Tab" });
  focusIs(button("Cancel"), "Tab from the last wraps to the first");
  await key(document.activeElement!, { key: "Tab", shiftKey: true });
  focusIs(button("Delete Workspace"), "Shift+Tab from the first wraps to the last");
});

test("a worktree another session shares is not offered for removal", async () => {
  const { asked } = await mount({ tree: "shared", changes: null });
  await chooseDelete("claude-1");
  const open = dialog()!;
  assert.equal(open.querySelector("h2")?.textContent, "Delete Session");
  const said = open.querySelector("#delete-dialog-description")?.textContent ?? "";
  assert.match(said, /Another session still works in this worktree, so its folder will not be deleted\./);
  assert.doesNotMatch(said, /from git/);
  assert.ok(document.querySelector(".delete-dialog-dirty") === null, "no files are at stake");
  button("Delete Session").click();
  await settle();
  assert.deepEqual(
    asked.at(-1),
    ["delete", "claude-1", { expected: "shared", expectedChanges: null, force: false }],
    "the session alone goes, and the host is told so",
  );
});

test("confirm waits for the host's answer, then takes the keyboard", async () => {
  let answer!: (preview: DeletePreview) => void;
  const asked: string[] = [];
  const flow = createDeleteFlow({
    preview: () => new Promise((resolve) => (answer = resolve)),
    remove: async (id) => void asked.push(id),
    toast() {},
  });
  const container = document.createElement("div");
  document.body.appendChild(container);
  flow.open({ action: "delete", session: SESSIONS[0], name: "Fix login" });
  cleanup = render(
    () =>
      createComponent(DeleteDialog, {
        get target() {
          return flow.target()!;
        },
        get state() {
          return flow.state();
        },
        onConfirm: flow.confirm,
        onClose: flow.close,
      }),
    container,
  );
  await settle();
  const confirm = document.querySelector<HTMLButtonElement>(".delete-dialog-confirm")!;
  assert.equal(confirm.disabled, true, "not while checking");
  confirm.click();
  await settle();
  assert.deepEqual(asked, []);
  answer({ tree: "removed", changes: 0 });
  await settle();
  assert.equal(confirm.disabled, false);
  focusIs(confirm, "the confirm takes the keyboard once it can be pressed");
});

test("a failed check says why and cannot be confirmed; Cancel keeps the keyboard", async () => {
  await mount(new Error("the session store could not be read: locked"));
  await chooseDelete("claude-1");
  assert.equal(document.querySelector('[role="alert"]')?.textContent, "the session store could not be read: locked");
  assert.equal(button("Delete").disabled, true);
  await key(document.activeElement!, { key: "Tab" });
  focusIs(button("Cancel"), "the one button that can be pressed");
});

test("the default flow asks the host's own delete commands, by session id", async () => {
  hostReplies.set("session_delete_preview", { tree: "removed", changes: 0 });
  await mount({ tree: "removed", changes: 0 }, "host");
  await chooseDelete("claude-1");
  assert.deepEqual(hostCalls.get("session_delete_preview"), { sessionId: "claude-1" });
  button("Delete Workspace").click();
  await settle();
  assert.deepEqual(hostCalls.get("session_delete"), {
    sessionId: "claude-1",
    expected: "removed",
    expectedChanges: 0,
    force: false,
  });
});

test("a delete the host refuses is said in a toast", async () => {
  hostReplies.set("session_delete_preview", { tree: "removed", changes: 0 });
  hostReplies.set("session_delete", new Error("The session was not deleted: busy"));
  const { toasts } = await mount({ tree: "removed", changes: 0 }, "host");
  await chooseDelete("claude-1");
  button("Delete Workspace").click();
  await settle();
  assert.deepEqual(toasts, ["The session was not deleted: busy"]);
});
