// The composer mounted for real: opened through a button (standing in for
// the sidebar's "+ New" and Mod+N, both of which just call the same open
// function, `sidebar.test.ts` and `tabs.test.ts` respectively prove that
// wiring), filled in, and submitted through the very `invoke` stub the
// window itself calls through.

import { hostCalls, hostReplies, settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent, createSignal, Show } from "solid-js";
import { render } from "solid-js/web";
import type { Session_Serialize, SessionsView_Serialize, Workspace_Serialize } from "../../../../ainb-app/bindings/AppState";
import { createComposerFlow, FOLLOW_MS } from "../../src/composer.ts";
import { Composer } from "../../src/composer.tsx";

function session(id: string, workspacePath: string): Session_Serialize {
  return { id, name: id, workspace_path: workspacePath, status: "Running", branch_name: `ainb/${id}` } as unknown as Session_Serialize;
}

function workspace(name: string, path: string, sessions: Session_Serialize[] = []): Workspace_Serialize {
  return { name, path, sessions, shell_session: null } as unknown as Workspace_Serialize;
}

function frame(): SessionsView_Serialize {
  return {
    workspaces: [workspace("repo", "/repo", [session("s-1", "/repo")])],
    selected_session_id: "s-1",
  } as unknown as SessionsView_Serialize;
}

/** What the flow did outside the view: toasts raised, focus restores, rows
 * selected. */
const effects = { toasts: [] as string[], restored: 0, selected: [] as string[] };

/** The session list the window draws; a test pushes the next frame. */
const [listed, setListed] = createSignal<SessionsView_Serialize>(frame());

/** The flow's clock, moved by hand. */
const clock = { now: 1_000 };

/**
 * The window's own flow (`createComposerFlow`, the code `main.tsx` runs),
 * mounted with its real `worktree_create` call going through the invoke stub.
 * The button stands in for Mod+N and "+ New", which only call `openComposer`.
 */
function Harness(props: { sessions: SessionsView_Serialize }) {
  const flow = createComposerFlow({
    toast: (message) => effects.toasts.push(message),
    restoreFocus: () => {
      effects.restored += 1;
    },
    sessions: listed,
    select: (sessionId) => effects.selected.push(sessionId),
    now: () => clock.now,
  });
  const button = document.createElement("button");
  button.type = "button";
  button.className = "open-composer";
  button.textContent = "New worktree";
  button.addEventListener("click", flow.openComposer);
  return [
    button,
    createComponent(Show, {
      get when() {
        return flow.open();
      },
      get children() {
        return createComponent(Composer, {
          get sessions() {
            return props.sessions;
          },
          get state() {
            return flow.state();
          },
          onSubmit: flow.submit,
          onClose: flow.closeComposer,
        });
      },
    }),
  ];
}

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  document.body.innerHTML = "";
  hostReplies.clear();
  hostCalls.clear();
  effects.toasts = [];
  effects.restored = 0;
  effects.selected = [];
  setListed(frame());
  clock.now = 1_000;
});

async function open(sessions: SessionsView_Serialize = frame()) {
  const container = document.createElement("div");
  document.body.appendChild(container);
  cleanup = render(() => createComponent(Harness, { sessions }), container);
  await settle();
  container.querySelector<HTMLButtonElement>(".open-composer")!.click();
  await settle();
  return container;
}

function fill(container: HTMLElement, selector: string, value: string) {
  const field = container.querySelector<HTMLInputElement | HTMLTextAreaElement>(selector)!;
  field.value = value;
  field.dispatchEvent(new window.Event("input", { bubbles: true }));
}

const submitButton = (container: HTMLElement) => container.querySelector<HTMLButtonElement>(".composer-create")!;

test("opening via the button mounts the composer over the default project", async () => {
  const container = await open();
  assert.ok(container.querySelector(".composer"), "the composer is mounted");
  assert.equal(container.querySelector<HTMLSelectElement>(".composer-project")?.value, "/repo");
});

test("with no session anywhere, the registered projects fill the Project select and create uses one", async () => {
  hostReplies.set("projects_list", [
    { name: "api", path: "/code/api" },
    { name: "web", path: "/code/web" },
  ]);
  hostReplies.set("worktree_create", {
    session_id: "u-1",
    tmux_session_name: "api-abcd1234",
    worktree_path: "/code/api/.worktrees/abcd1234",
    branch: "ainb/first",
  });
  const container = await open({ workspaces: [], selected_session_id: null } as unknown as SessionsView_Serialize);
  const select = container.querySelector<HTMLSelectElement>(".composer-project")!;
  assert.deepEqual(
    [...select.options].map((option) => [option.textContent, option.value]),
    [
      ["api", "/code/api"],
      ["web", "/code/web"],
    ],
  );
  assert.equal(select.value, "/code/api", "the first registered project is picked");
  assert.equal(container.querySelector(".composer-error"), null, "no 'Choose a project' with one to choose");

  fill(container, ".composer-name", "first");
  submitButton(container).click();
  await settle();
  assert.equal((hostCalls.get("worktree_create") as { args: { repo_path: string } }).args.repo_path, "/code/api");
});

test("with no project at all, the composer says how to register a folder", async () => {
  hostReplies.set("projects_list", []);
  const container = await open({ workspaces: [], selected_session_id: null } as unknown as SessionsView_Serialize);
  const hint = container.querySelector(".composer-empty-projects");
  assert.ok(hint, "an empty state, not only 'Choose a project.'");
  assert.match(hint.textContent ?? "", /add your repositories' folder to workspace_defaults\.workspace_scan_paths/i);
  assert.match(hint.textContent ?? "", /replaces the whole list/, "the command's effect is said, not left to surprise");
  assert.equal(
    hint.querySelector("code")?.textContent,
    `ainb config set workspace_defaults.workspace_scan_paths '["~/code"]'`,
  );
  assert.equal(submitButton(container).disabled, true);
});

test("the empty state's Add folder opens the same picker and lands on the new project", async () => {
  hostReplies.set("projects_list", []);
  const container = await open({ workspaces: [], selected_session_id: null } as unknown as SessionsView_Serialize);
  hostReplies.set("project_add", { name: "first", path: "/code/first" });
  hostReplies.set("projects_list", [{ name: "first", path: "/code/first" }]);
  assert.equal(
    container.querySelectorAll(".composer-add-project, .composer-add-folder").length,
    1,
    "one add button in the empty state",
  );
  container.querySelector<HTMLButtonElement>(".composer-empty-projects .composer-add-folder")!.click();
  await settle();

  assert.ok(hostCalls.has("project_add"));
  assert.equal(container.querySelector<HTMLSelectElement>(".composer-project")?.value, "/code/first");
  assert.equal(container.querySelector(".composer-empty-projects"), null, "no longer empty");
});

test("a refusal under the Project field clears when another project is chosen", async () => {
  hostReplies.set("projects_list", [{ name: "other", path: "/code/other" }]);
  hostReplies.set("project_add", new Error("/tmp/x is not the top folder of a git repository."));
  const container = await open();
  container.querySelector<HTMLButtonElement>(".composer-add-project")!.click();
  await settle();
  assert.ok(container.querySelector(".composer-add-error"));

  const select = container.querySelector<HTMLSelectElement>(".composer-project")!;
  select.value = "/code/other";
  select.dispatchEvent(new window.Event("change", { bubbles: true }));
  await settle();
  assert.equal(container.querySelector(".composer-add-error"), null);
});

test("the register hint is gone once there is a project", async () => {
  const container = await open();
  assert.equal(container.querySelector(".composer-empty-projects"), null);
});

test("Add project registers the picked folder and selects it", async () => {
  const container = await open();
  hostReplies.set("project_add", { name: "fresh", path: "/code/fresh" });
  hostReplies.set("projects_list", [{ name: "fresh", path: "/code/fresh" }]);
  hostCalls.delete("projects_list");
  container.querySelector<HTMLButtonElement>(".composer-add-project")!.click();
  await settle();

  assert.ok(hostCalls.has("project_add"), "the host's picker was asked for");
  assert.ok(hostCalls.has("projects_list"), "the list is read again from the host");
  const select = container.querySelector<HTMLSelectElement>(".composer-project")!;
  assert.deepEqual(
    [...select.options].map((option) => option.value),
    ["/repo", "/code/fresh"],
  );
  assert.equal(select.value, "/code/fresh", "the new project is the one picked");
});

test("a refetch that drops the picked project moves the pick to a listed one before anything is sent", async () => {
  hostReplies.set("worktree_create", {
    session_id: "u-1",
    tmux_session_name: "repo-abcd1234",
    worktree_path: "/repo/.worktrees/abcd1234",
    branch: "ainb/fix-login",
  });
  const container = await open();
  // The host registers the folder, then its own list no longer carries it.
  hostReplies.set("project_add", { name: "gone", path: "/code/gone" });
  hostReplies.set("projects_list", []);
  container.querySelector<HTMLButtonElement>(".composer-add-project")!.click();
  await settle();

  fill(container, ".composer-name", "Fix login");
  submitButton(container).click();
  await settle();
  assert.equal(
    (hostCalls.get("worktree_create") as { args: { repo_path: string } } | undefined)?.args.repo_path,
    "/repo",
    "the first listed project, never the dropped path",
  );
});

test("a cancelled picker changes nothing, a refused folder says why", async () => {
  hostReplies.set("project_add", null);
  const container = await open();
  const add = container.querySelector<HTMLButtonElement>(".composer-add-project")!;
  add.click();
  await settle();
  assert.equal(container.querySelector<HTMLSelectElement>(".composer-project")?.value, "/repo");
  assert.equal(container.querySelector(".composer-add-error"), null);

  hostReplies.set("project_add", new Error("/tmp/x is not the top folder of a git repository: pick the repository's own folder."));
  add.click();
  await settle();
  assert.equal(
    container.querySelector(".composer-add-error")?.textContent,
    "/tmp/x is not the top folder of a git repository: pick the repository's own folder.",
  );
  assert.equal(container.querySelector<HTMLSelectElement>(".composer-project")?.value, "/repo", "the pick is kept");
});

test("a host without the projects command still offers the frame's projects", async () => {
  hostReplies.set("projects_list", new Error("command projects_list not found"));
  const container = await open();
  assert.equal(container.querySelector<HTMLSelectElement>(".composer-project")?.value, "/repo");
});

test("filling the form and submitting calls worktree_create with the exact args", async () => {
  hostReplies.set("worktree_create", {
    session_id: "u-1",
    tmux_session_name: "repo-abcd1234",
    worktree_path: "/repo/.worktrees/abcd1234",
    branch: "ainb/fix-login",
  });
  const container = await open();
  fill(container, ".composer-name", "Fix login");
  fill(container, ".composer-prompt", "fix the login bug");
  submitButton(container).click();
  await settle();

  assert.deepEqual(hostCalls.get("worktree_create"), {
    args: {
      repo_path: "/repo",
      branch: "ainb/fix-login",
      base: null,
      agent: "claude",
      model: null,
      prompt: "fix the login bug",
    },
  });
});

test("a successful create closes the composer", async () => {
  hostReplies.set("worktree_create", {
    session_id: "u-1",
    tmux_session_name: "repo-abcd1234",
    worktree_path: "/repo/.worktrees/abcd1234",
    branch: "ainb/fix-login",
  });
  const container = await open();
  fill(container, ".composer-name", "Fix login");
  submitButton(container).click();
  await settle();

  assert.equal(container.querySelector(".composer"), null, "the overlay is gone");
});

test("a refused create shows the host's sentence and keeps the typed fields", async () => {
  hostReplies.set("worktree_create", new Error("branch ainb/fix-login already exists"));
  const container = await open();
  fill(container, ".composer-name", "Fix login");
  fill(container, ".composer-prompt", "fix it");
  submitButton(container).click();
  await settle();

  const failure = container.querySelector('[role="alert"]');
  assert.equal(failure?.textContent, "branch ainb/fix-login already exists");
  assert.equal(container.querySelector<HTMLInputElement>(".composer-name")?.value, "Fix login");
  assert.equal(container.querySelector<HTMLInputElement>(".composer-name")?.disabled, false, "fields are editable again");
  assert.ok(container.querySelector(".composer"), "the overlay stayed open");
});

test("Esc closes the composer even after focus left every field, and focus is restored", async () => {
  const container = await open();
  (document.activeElement as HTMLElement | null)?.blur();
  document.body.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  await settle();
  assert.equal(container.querySelector(".composer"), null);
  assert.equal(effects.restored, 1, "the keyboard went back where it was");
});

test("Esc while an input method is composing does not close", async () => {
  const container = await open();
  document.body.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true, isComposing: true }));
  await settle();
  assert.ok(container.querySelector(".composer"), "the candidate is cancelled, not the form");
});

test("Mod+Enter in the prompt submits, but not while an input method is composing", async () => {
  hostReplies.set("worktree_create", {
    session_id: "u-1",
    tmux_session_name: "repo-abcd1234",
    worktree_path: "/repo/.worktrees/abcd1234",
    branch: "ainb/fix-login",
  });
  const container = await open();
  fill(container, ".composer-name", "Fix login");
  const prompt = container.querySelector<HTMLTextAreaElement>(".composer-prompt")!;
  fill(container, ".composer-prompt", "fix it");

  // The IME's Enter confirms a candidate: nothing is sent.
  prompt.dispatchEvent(
    new window.KeyboardEvent("keydown", { key: "Enter", ctrlKey: true, metaKey: true, isComposing: true, bubbles: true, cancelable: true }),
  );
  await settle();
  assert.equal(hostCalls.get("worktree_create"), undefined, "a composing Enter never submits");

  prompt.dispatchEvent(
    new window.KeyboardEvent("keydown", { key: "Enter", ctrlKey: true, metaKey: true, bubbles: true, cancelable: true }),
  );
  await settle();
  assert.ok(hostCalls.get("worktree_create"), "the same chord, composition over, submits");
});

test("a drag that starts in a field and ends on the scrim does not close", async () => {
  const container = await open();
  const backdrop = container.querySelector<HTMLElement>(".composer-backdrop")!;
  container.querySelector(".composer-name")!.dispatchEvent(new window.MouseEvent("mousedown", { bubbles: true }));
  backdrop.dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
  await settle();
  assert.ok(container.querySelector(".composer"), "released outside, nothing thrown away");

  backdrop.dispatchEvent(new window.MouseEvent("mousedown", { bubbles: true }));
  backdrop.dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
  await settle();
  assert.equal(container.querySelector(".composer"), null, "a real click on the scrim closes");
});

test("Tab from the last control wraps to the first, Shift+Tab the other way", async () => {
  const container = await open();
  const form = container.querySelector<HTMLFormElement>("form.composer")!;
  const controls = [
    ...form.querySelectorAll<HTMLElement>(
      "button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), summary",
    ),
  ];
  const first = controls[0];
  const last = controls[controls.length - 1];
  last.focus();
  last.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Tab", bubbles: true, cancelable: true }));
  assert.equal(document.activeElement, first);
  first.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Tab", shiftKey: true, bubbles: true, cancelable: true }));
  assert.equal(document.activeElement, last);
});

test("a create that fails after Cancel is raised as a toast, not lost", async () => {
  hostReplies.set("worktree_create", new Error("branch exists"));
  const container = await open();
  fill(container, ".composer-name", "Fix login");
  submitButton(container).click();
  container.querySelector<HTMLButtonElement>(".composer-cancel")?.click();
  await settle();
  assert.deepEqual(effects.toasts, ["branch exists"]);
});

test("Cancel closes the view without waiting for the host to answer", async () => {
  hostReplies.set("worktree_create", {
    session_id: "u-1",
    tmux_session_name: "repo-abcd1234",
    worktree_path: "/repo/.worktrees/abcd1234",
    branch: "ainb/fix-login",
  });
  const container = await open();
  fill(container, ".composer-name", "Fix login");
  submitButton(container).click();
  // Cancelled before the invoke stub's promise has a chance to settle
  // (no `settle()` in between): the view must close on the click alone, the
  // same tick, rather than waiting on the request it no longer shows.
  container.querySelector<HTMLButtonElement>(".composer-cancel")?.click();
  assert.equal(container.querySelector(".composer"), null, "the view closed on Cancel alone");

  // The request itself is not this component's to stop; let it settle so
  // the test leaves no pending timer or unhandled rejection behind.
  await settle();
});

test("an invalid base ref marks the field invalid and disables Create", async () => {
  const container = await open();
  fill(container, ".composer-base", "a b");
  await settle();

  const base = container.querySelector<HTMLInputElement>(".composer-base");
  assert.equal(base?.getAttribute("aria-invalid"), "true");
  assert.equal(submitButton(container).disabled, true);

  fill(container, ".composer-base", "main");
  await settle();
  assert.equal(base?.getAttribute("aria-invalid"), null);
  assert.equal(submitButton(container).disabled, false);
});

/** The frame after the daemon listed the created session `u-2`, with
 * `selected` the session list's selection. */
function withCreated(selected = "s-1"): SessionsView_Serialize {
  return {
    workspaces: [workspace("repo", "/repo", [session("s-1", "/repo"), session("s-3", "/repo"), session("u-2", "/repo")])],
    selected_session_id: selected,
  } as unknown as SessionsView_Serialize;
}

/** The frame before `u-2` is listed, with `selected` selected. */
function beforeCreated(selected: string): SessionsView_Serialize {
  return {
    workspaces: [workspace("repo", "/repo", [session("s-1", "/repo"), session("s-3", "/repo")])],
    selected_session_id: selected,
  } as unknown as SessionsView_Serialize;
}

const createdReply = {
  session_id: "u-2",
  tmux_session_name: "repo-u2",
  worktree_path: "/repo/.worktrees/u2",
  branch: "ainb/fix-login",
};

/** Open the composer, create `u-2`, and let the host answer. */
async function create() {
  hostReplies.set("worktree_create", createdReply);
  const container = await open();
  fill(container, ".composer-name", "Fix login");
  submitButton(container).click();
  await settle();
}

test("a created session is selected once the session list carries it, and only once", async () => {
  await create();
  assert.deepEqual(effects.selected, [], "not listed yet: nothing to select, and the old row is not reselected");

  setListed(withCreated());
  await settle();
  assert.deepEqual(effects.selected, ["u-2"], "the new session takes the selection");

  setListed({ ...withCreated() });
  await settle();
  assert.deepEqual(effects.selected, ["u-2"], "a later frame does not select it again over a person's pick");
});

test("a created row that lands after the deadline does not take the selection", async () => {
  await create();
  clock.now += FOLLOW_MS + 1;
  setListed(withCreated());
  await settle();
  assert.deepEqual(effects.selected, []);
});

// A board card and a palette row both select through `session_list.select_row`
// (`showIntents`, `palette.ts`), so either reaches the flow only as the list's
// selection moving. Each must beat a created row that lands later.
test("a selection that moves before the created row lands keeps it, from the board or the palette", async () => {
  for (const source of ["board card", "palette row"]) {
    effects.selected = [];
    cleanup?.();
    document.body.innerHTML = "";
    setListed(frame());
    await create();
    setListed(beforeCreated("s-3"));
    await settle();
    setListed(withCreated("s-3"));
    await settle();
    assert.deepEqual(effects.selected, [], `a ${source} pick wins`);
  }
});
