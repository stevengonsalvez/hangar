// The sidebar row's context menu, mounted: a right-click or the keyboard opens
// it at the row, each item reaches the window's existing action for it through
// `runRowPick`, Esc gives the keyboard back to the row, and an item the row
// cannot take is drawn but cannot be chosen. `src/row_menu.test.ts` holds the
// item list and the key rules; this proves the sidebar actually draws and
// wires them.

import { settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent, createSignal } from "solid-js";
import { render } from "solid-js/web";
import type { Session_Serialize, SessionsView_Serialize } from "../../../../ainb-app/bindings/AppState";
import { runRowPick, type RowPick } from "../../src/row_menu.ts";
import { Sidebar } from "../../src/sidebar.tsx";
import type { RendererIntent } from "../../src/tabs.ts";

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

/** Two sessions sharing one worktree, plus one on a remote host. */
const SESSIONS = [
  session("claude-1", "/repo/wt-a", { display_name: "Fix login" }),
  session("shell-1", "/repo/wt-a", { agent_type: "Shell", created_at: "2023-01-01T00:00:00Z" }),
  session("remote-1", "/srv/wt-b", { ssh_target: { host: "box" } as Session_Serialize["ssh_target"] }),
];

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  document.body.innerHTML = "";
});

/** What each existing action was asked to do, in order. */
type Call = ["open", string] | ["run", RendererIntent[]] | ["copy", string];

/** Swap the mounted sidebar's frame, as the next drain would. */
let setFrame: (next: Session_Serialize[]) => void = () => undefined;

/** Mount the sidebar with `main.tsx`'s own pick runner over recording deps. */
async function mount(sessions = SESSIONS) {
  const calls: Call[] = [];
  const [held, setHeld] = createSignal(frame(sessions));
  setFrame = (next) => setHeld(frame(next));
  const container = document.createElement("div");
  document.body.appendChild(container);
  cleanup = render(
    () =>
      createComponent(Sidebar, {
        get sessions() {
          return held();
        },
        stale: false,
        loading: false,
        pending: null,
        onOpen: (id: string) => calls.push(["open", id]),
        onNew() {},
        ref() {},
        onRowPick: (pick: RowPick) =>
          runRowPick(pick, {
            open: (id) => calls.push(["open", id]),
            run: (intents) => calls.push(["run", intents]),
            copy: (text) => calls.push(["copy", text]),
            reselect: () => [],
          }),
      }),
    container,
  );
  await settle();
  return calls;
}

const row = (id: string) => document.querySelector<HTMLButtonElement>(`.session-row[data-session="${id}"]`)!;
const menu = () => document.querySelector<HTMLElement>('[role="menu"]');
const item = (label: string) =>
  [...document.querySelectorAll<HTMLButtonElement>('[role="menuitem"]')].find((el) => el.textContent === label)!;

/** Whether `expected` has the keyboard. Checked by identity: a failing
 * `assert.equal` on two DOM nodes has node inspect both whole, which on a
 * happy-dom tree runs for minutes. */
function focusIs(expected: Element | undefined, message = "focus") {
  const active = document.activeElement;
  assert.ok(active === expected, `${message}: focus is on ${active?.outerHTML.slice(0, 80) ?? "nothing"}`);
}

/** A right-click as a browser sends it: the press, then the menu event. */
async function rightClick(target: Element, x = 40, y = 60) {
  target.dispatchEvent(new window.MouseEvent("pointerdown", { bubbles: true, button: 2 }) as unknown as Event);
  const event = new window.MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: x, clientY: y });
  target.dispatchEvent(event as unknown as Event);
  await settle();
  return event;
}

async function key(target: Element, init: KeyboardEventInit) {
  const event = new window.KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init });
  target.dispatchEvent(event as unknown as Event);
  await settle();
  return event;
}

test("a right-click on a row opens the menu at the pointer, instead of the webview's own", async () => {
  await mount();
  const event = await rightClick(row("shell-1"), 40, 60);
  assert.equal(event.defaultPrevented, true, "the webview's own menu is suppressed");
  const open = menu();
  assert.ok(open, "a menu is drawn");
  assert.equal(open.style.left, "40px");
  assert.equal(open.style.top, "60px");
  assert.deepEqual(
    [...open.querySelectorAll('[role="menuitem"]')].map((el) => el.textContent),
    ["Open", "Open in Editor", "Copy Path", "Copy Worktree Name"],
  );
  focusIs(item("Open"), "the first item takes the keyboard");
});

test("each item runs the window's existing action for the row it was opened on, then closes", async () => {
  const cases: Array<[string, Call]> = [
    ["Open", ["open", "shell-1"]],
    [
      "Open in Editor",
      [
        "run",
        [
          { Command: ["session_list.select_row", { target: { session: "shell-1" }, open: false }] },
          { Command: ["session_list.editor", null] },
        ],
      ],
    ],
    ["Copy Path", ["copy", "/repo/wt-a"]],
    // The worktree's name as its card shows it: the newest session's label.
    ["Copy Worktree Name", ["copy", "Fix login"]],
  ];
  for (const [label, expected] of cases) {
    const calls = await mount();
    await rightClick(row("shell-1"));
    item(label).click();
    await settle();
    assert.deepEqual(calls, [expected], label);
    assert.ok(menu() === null, `${label} closes the menu`);
    focusIs(row("shell-1"), `${label} gives the keyboard back to the row`);
    cleanup?.();
    cleanup = undefined;
    document.body.innerHTML = "";
  }
});

test("Esc closes the menu and returns focus to the row that opened it", async () => {
  const calls = await mount();
  await rightClick(row("claude-1"));
  await key(document.activeElement!, { key: "Escape" });
  assert.ok(menu() === null, "the menu closed");
  focusIs(row("claude-1"));
  assert.deepEqual(calls, [], "closing runs nothing");
});

test("a right-click on the card's title targets the card's first row and returns focus there", async () => {
  const calls = await mount();
  const title = document.querySelector(".worktree-card .worktree-card-title")!;
  await rightClick(title);
  assert.ok(menu());
  item("Open").click();
  await settle();
  assert.deepEqual(calls, [["open", "claude-1"]]);
  focusIs(row("claude-1"));
});

test("the context-menu key and Shift+F10 open the menu from a focused row, and arrows walk it", async () => {
  await mount();
  row("claude-1").focus();
  const pressed = await key(row("claude-1"), { key: "ContextMenu" });
  assert.equal(pressed.defaultPrevented, true);
  assert.ok(menu(), "the context-menu key opens it");
  focusIs(item("Open"));
  await key(document.activeElement!, { key: "ArrowDown" });
  focusIs(item("Open in Editor"));
  await key(document.activeElement!, { key: "ArrowUp" });
  await key(document.activeElement!, { key: "ArrowUp" });
  focusIs(item("Copy Worktree Name"), "up from the first wraps to the last");
  await key(document.activeElement!, { key: "End" });
  focusIs(item("Copy Worktree Name"));
  await key(document.activeElement!, { key: "Home" });
  focusIs(item("Open"));
  await key(document.activeElement!, { key: "Escape" });
  focusIs(row("claude-1"));

  await key(row("claude-1"), { key: "F10", shiftKey: true });
  assert.ok(menu(), "Shift+F10 opens it too");
});

test("a row that cannot take an action draws it disabled, skips it, and never runs it", async () => {
  const calls = await mount();
  await rightClick(row("remote-1"));
  const editor = item("Open in Editor");
  assert.equal(editor.getAttribute("aria-disabled"), "true");
  assert.match(editor.title, /local/i, "the tooltip says why");
  editor.click();
  await settle();
  assert.deepEqual(calls, [], "a disabled item runs nothing");
  assert.ok(menu(), "and the menu stays open");
  item("Open").focus();
  await key(document.activeElement!, { key: "ArrowDown" });
  focusIs(item("Copy Path"), "the arrows skip the disabled item");
});

test("a row with no worktree path cannot copy or open a path", async () => {
  await mount([session("bare-1", "")]);
  await rightClick(row("bare-1"));
  assert.equal(item("Copy Path").getAttribute("aria-disabled"), "true");
  assert.equal(item("Open in Editor").getAttribute("aria-disabled"), "true");
  assert.equal(item("Copy Worktree Name").getAttribute("aria-disabled"), null);
});

test("a press outside the menu, or Tab, closes it", async () => {
  await mount();
  await rightClick(row("claude-1"));
  assert.ok(menu(), "the menu is open before the press");
  document.body.dispatchEvent(new window.MouseEvent("pointerdown", { bubbles: true }) as unknown as Event);
  await settle();
  assert.ok(menu() === null, "a press outside closes it");

  await rightClick(row("claude-1"));
  assert.ok(menu(), "the menu is open before Tab");
  await key(document.activeElement!, { key: "Tab" });
  assert.ok(menu() === null, "Tab closes it");
});

test("a second right-click moves the one menu to the new row", async () => {
  const calls = await mount();
  await rightClick(row("claude-1"), 10, 10);
  await rightClick(row("shell-1"), 90, 120);
  assert.equal(document.querySelectorAll('[role="menu"]').length, 1);
  assert.equal(menu()!.style.top, "120px");
  item("Open").click();
  await settle();
  assert.deepEqual(calls, [["open", "shell-1"]]);
});

test("the menu closes when its session leaves the list, so no item acts on a row that is gone", async () => {
  await mount();
  await rightClick(row("remote-1"));
  assert.ok(menu(), "open on the remote row");
  setFrame(SESSIONS.filter((candidate) => candidate.id !== "remote-1"));
  await settle();
  assert.ok(menu() === null, "the menu went with its row");
  focusIs(document.querySelector(".sidebar")!, "the keyboard goes back to the sidebar");

  await rightClick(row("claude-1"));
  setFrame([...SESSIONS]);
  await settle();
  assert.ok(menu(), "a frame that still carries the row leaves the menu open");
});

test("a row says it has a menu", async () => {
  await mount();
  assert.equal(row("claude-1").getAttribute("aria-haspopup"), "menu");
});

test("the keyboard leaving the menu for another control closes it and leaves the keyboard there", async () => {
  await mount();
  const field = document.createElement("input");
  document.body.appendChild(field);
  await rightClick(row("claude-1"));
  assert.ok(menu(), "open");
  field.focus();
  await settle();
  assert.ok(menu() === null, "the palette or composer taking the keyboard closes the menu");
  focusIs(field, "the keyboard stays where it went");
});

test("a press outside closes the menu without pulling the keyboard back to the row", async () => {
  await mount();
  const field = document.createElement("input");
  document.body.appendChild(field);
  await rightClick(row("claude-1"));
  field.dispatchEvent(new window.MouseEvent("pointerdown", { bubbles: true }) as unknown as Event);
  await settle();
  assert.ok(menu() === null);
  // The press's own default action moves the keyboard to what was pressed;
  // the menu must not have pulled it to the row first.
  assert.ok(document.activeElement !== row("claude-1"), "the row did not take the keyboard");
});

test("the window losing focus or resizing closes the menu", async () => {
  await mount();
  for (const type of ["blur", "resize"]) {
    await rightClick(row("claude-1"));
    assert.ok(menu(), `open before ${type}`);
    window.dispatchEvent(new window.Event(type) as unknown as Event);
    await settle();
    assert.ok(menu() === null, `${type} closes it`);
  }
});

test("scrolling the sidebar closes the menu, which would no longer sit at its row", async () => {
  await mount();
  await rightClick(row("claude-1"));
  assert.ok(menu(), "open before the scroll");
  // A scroll does not bubble: the menu hears it by capture on the document.
  document.querySelector(".sidebar")!.dispatchEvent(new window.Event("scroll") as unknown as Event);
  await settle();
  assert.ok(menu() === null, "the scroll closes it");
  assert.ok(document.activeElement !== row("claude-1"), "without pulling the keyboard to the row");
});

test("a right-click near the window's corner keeps the whole menu on screen", async () => {
  const proto = window.HTMLElement.prototype as unknown as { getBoundingClientRect(): unknown };
  const real = proto.getBoundingClientRect;
  proto.getBoundingClientRect = function (this: Element) {
    return this.getAttribute("role") === "menu"
      ? { left: 0, top: 0, right: 200, bottom: 120, width: 200, height: 120, x: 0, y: 0 }
      : real.call(this);
  };
  try {
    await mount();
    await rightClick(row("claude-1"), window.innerWidth - 10, window.innerHeight - 10);
    assert.equal(menu()!.style.left, `${window.innerWidth - 200}px`);
    assert.equal(menu()!.style.top, `${window.innerHeight - 120}px`);
  } finally {
    proto.getBoundingClientRect = real;
  }
});
