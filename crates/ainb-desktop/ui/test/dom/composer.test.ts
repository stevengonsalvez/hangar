// The composer mounted for real: opened through a button (standing in for
// the sidebar's "+ New" and Mod+N, both of which just call the same open
// function, `sidebar.test.ts` and `tabs.test.ts` respectively prove that
// wiring), filled in, and submitted through the very `invoke` stub the
// window itself calls through.

import { hostCalls, hostReplies, settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent, Show } from "solid-js";
import { render } from "solid-js/web";
import type { Session_Serialize, SessionsView_Serialize, Workspace_Serialize } from "../../../../ainb-app/bindings/AppState";
import { createComposerFlow } from "../../src/composer.ts";
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

/** What the flow did outside the view: toasts raised, focus restores. */
const effects = { toasts: [] as string[], restored: 0 };

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
});

async function open() {
  const container = document.createElement("div");
  document.body.appendChild(container);
  cleanup = render(() => createComponent(Harness, { sessions: frame() }), container);
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
      name: null,
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
