// The whole window, mounted over a fake host, with the row menu's Delete open:
// the real `main.tsx` makes the shell behind the dialog inert, runs no chord
// under it (Mod+N would open the composer over it), closes it on Esc with the
// keyboard back on the row, and sends the host's delete once, only from the
// dialog's own confirm. `delete_dialog.test.ts` drives the dialog alone; this
// is the wiring the real window gives it (the `settings_tab.test.ts` harness).

import "./window.ts";

import assert from "node:assert/strict";
import { test } from "node:test";

type Callback = (payload: unknown) => void;

const HOST = "host-1";

/** The fake host: what the window asked of it, and its frame channel. */
const host = {
  version: 1,
  calls: [] as Array<[string, unknown]>,
  frames: undefined as undefined | { onmessage: Callback },
  events: new Map<string, Callback[]>(),
  preview: { tree: "removed", changes: 0 } as { tree: string; changes: number | null },
  /** When set, the next preview waits for `answer` instead of `preview`. */
  hold: false,
  answer: undefined as undefined | ((preview: { tree: string; changes: number | null }) => void),
};

function frames() {
  host.version += 1;
  const row = (id: string, name: string) => ({
    id,
    name,
    status: "Running",
    branch_name: `ainb/${name}`,
    workspace_path: `/repo/${name}`,
    mode: "Interactive",
    agent_type: "Claude",
    ssh_target: null,
    git_changes: { added: 0, modified: 0, deleted: 0 },
    created_at: "2024-01-01T00:00:00Z",
    attention: [],
  });
  const at = { version: host.version, epoch: 1, host_id: HOST, daemon_read: null };
  return {
    frames: [
      { section: "shell", ...at, body: { current_screen: "session_list", previous_screen: null, notifications: [] } },
      {
        section: "sessions",
        ...at,
        body: {
          workspaces: [{ name: "repo", path: "/repo", shell_session: null, sessions: [row("u-1", "api"), row("u-2", "web")] }],
          selected_session_id: null,
          shell_selected: false,
        },
      },
    ],
  };
}

const callbacks = new Map<number, Callback>();
let nextCallback = 1;
(window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {
  transformCallback: (callback: Callback) => {
    callbacks.set(nextCallback, callback);
    return nextCallback++;
  },
  unregisterCallback: () => undefined,
  invoke: async (command: string, args: Record<string, unknown> = {}) => {
    switch (command) {
      case "plugin:event|listen": {
        const handlers = host.events.get(args.event as string) ?? [];
        handlers.push(callbacks.get(args.handler as number)!);
        host.events.set(args.event as string, handlers);
        return args.handler;
      }
      case "sidecar_state":
        return { state: "running" };
      case "terminal_tabs":
        return { tabs: [], focus: null };
      // What a composer, wrongly opened over the dialog, would ask for.
      case "projects_list":
        return [];
      case "subscribe":
        host.frames = args.frames as { onmessage: Callback };
        host.frames.onmessage(frames());
        return HOST;
      case "session_delete_preview":
      case "session_delete":
        host.calls.push([command, args]);
        if (command === "session_delete_preview" && host.hold) {
          host.hold = false;
          return new Promise((resolve) => (host.answer = resolve));
        }
        return command === "session_delete_preview" ? host.preview : null;
      default:
        return null;
    }
  },
};

// The window's own globals happy-dom does not hand Node by default.
for (const name of ["requestAnimationFrame", "cancelAnimationFrame", "ResizeObserver", "getComputedStyle", "localStorage"]) {
  const value = (window as unknown as Record<string, unknown>)[name];
  Object.defineProperty(globalThis, name, {
    value: typeof value === "function" && name !== "ResizeObserver" ? (value as () => unknown).bind(window) : value,
    configurable: true,
    writable: true,
  });
}
Object.defineProperty(window, "matchMedia", {
  value: () => ({ matches: true, addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {} }),
  configurable: true,
  writable: true,
});

async function until(ready: () => boolean, what: string): Promise<void> {
  for (let turn = 0; turn < 200; turn += 1) {
    if (ready()) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  assert.fail(`timed out waiting for ${what}`);
}

const row = (id: string) => document.querySelector<HTMLElement>(`.session-row[data-session="${id}"]`);
const dialog = () => document.querySelector<HTMLElement>('[role="alertdialog"]');
const shell = () => document.querySelector<HTMLElement>(".shell-content")!;
const composer = () => document.querySelector(".composer");
const button = (text: string) =>
  [...document.querySelectorAll<HTMLButtonElement>(".delete-dialog button")].find((el) => el.textContent === text);

function focusIs(expected: Element | null | undefined, message: string) {
  const active = document.activeElement;
  assert.ok(active === expected, `${message}: focus is on ${active?.outerHTML.slice(0, 80) ?? "nothing"}`);
}

/** Right-click `id`'s row and choose Delete, as a person does, and wait
 * for the dialog; for its answer too unless `pending`. */
async function openDelete(id: string, pending = false): Promise<void> {
  await until(() => row(id) !== null, "the sidebar row");
  row(id)!.focus();
  row(id)!.dispatchEvent(new window.MouseEvent("contextmenu", { bubbles: true, cancelable: true }) as unknown as Event);
  await until(() => document.querySelector('[role="menu"]') !== null, "the row menu");
  const item = [...document.querySelectorAll<HTMLElement>('[role="menuitem"]')].find((el) => el.textContent === "Delete");
  assert.ok(item, "the menu offers Delete");
  item.click();
  if (pending) await until(() => dialog() !== null, "the dialog, still counting");
  else await until(() => button("Delete Workspace")?.disabled === false, "the dialog, ready");
}

test("Delete in the real window: inert behind, no Mod+N, Esc closes, focus back on the row", async () => {
  const root = document.createElement("div");
  root.id = "root";
  document.body.appendChild(root);
  await import("../../src/main.tsx");

  await openDelete("u-2");
  assert.ok(dialog(), "the confirmation is drawn");
  assert.equal(shell().hasAttribute("inert"), true, "the shell behind the dialog is inert");
  assert.ok(!shell().contains(dialog()), "and the dialog is not inside it");

  // Mod+N off macOS is Ctrl+Shift+N; under the dialog it opens nothing.
  window.dispatchEvent(
    new window.KeyboardEvent("keydown", {
      code: "KeyN",
      key: "N",
      ctrlKey: true,
      shiftKey: true,
      bubbles: true,
      cancelable: true,
    }) as unknown as Event,
  );
  await new Promise((resolve) => setTimeout(resolve, 50));
  assert.equal(composer(), null, "no composer over the dialog");
  assert.ok(dialog(), "the dialog is still open");

  document.activeElement!.dispatchEvent(
    new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }) as unknown as Event,
  );
  await until(() => dialog() === null, "Esc to close the dialog");
  await until(() => !shell().hasAttribute("inert"), "the shell to come back");
  await until(() => document.activeElement === row("u-2"), "the keyboard back on the row");
  focusIs(row("u-2"), "the row that opened it");
  assert.deepEqual(
    host.calls.map(([command]) => command),
    ["session_delete_preview"],
    "asked what goes, deleted nothing",
  );
});

test("Delete in the real window: the confirm sends the host's delete once, with what it showed", async () => {
  host.calls = [];
  await openDelete("u-1");
  button("Delete Workspace")!.click();
  await until(() => dialog() === null, "the dialog to close as the delete starts");
  await new Promise((resolve) => setTimeout(resolve, 50));
  assert.deepEqual(host.calls, [
    ["session_delete_preview", { sessionId: "u-1" }],
    ["session_delete", { sessionId: "u-1", expected: "removed", expectedChanges: 0, force: false }],
  ]);
  assert.equal(shell().hasAttribute("inert"), false);
});

test("Delete in the real window: a press while the count is out deletes nothing; a dirty count asks for Force Delete", async () => {
  host.calls = [];
  host.hold = true;
  await openDelete("u-2", true);
  await new Promise((resolve) => setTimeout(resolve, 50));
  focusIs(button("Cancel"), "Cancel holds the keyboard while the host counts");
  // The confirm, where a reflexive press lands if the keyboard is anywhere
  // but Cancel: it must not start a delete before the count is known.
  button("Delete")!.click();
  await new Promise((resolve) => setTimeout(resolve, 50));
  assert.deepEqual(
    host.calls.map(([command]) => command),
    ["session_delete_preview"],
    "nothing deleted while counting",
  );
  assert.ok(dialog(), "the dialog is still open");

  host.answer!({ tree: "removed", changes: 3 });
  await until(() => button("Force Delete") !== undefined, "the dirty count");
  assert.deepEqual(
    [...document.querySelectorAll<HTMLButtonElement>(".delete-dialog button")].map((el) => el.textContent),
    ["Cancel", "Force Delete"],
  );
  focusIs(button("Cancel"), "the keyboard stays on Cancel for Force Delete");
  button("Cancel")!.click();
  await until(() => dialog() === null, "Cancel to close the dialog");
  assert.deepEqual(
    host.calls.map(([command]) => command),
    ["session_delete_preview"],
    "and still nothing deleted",
  );
});
