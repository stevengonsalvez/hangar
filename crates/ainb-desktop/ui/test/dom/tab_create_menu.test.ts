// The tab strip's one "+" mounted for real, as Orca's "New tab" menu: New
// terminal, a separator, then the agents. Both items run the window's own
// flows (`createShellTabs`, `createNewAgentFlow`, the code `main.tsx` runs)
// through the very `invoke` stub the window calls through, on the target
// `worktreeTarget` picks. After an agent is added the host opens and focuses its
// tab, and the flow moves the session list's selection onto the new session
// once it is listed, the same rule a new worktree follows (`createFollow`).

import { hostCalls, hostReplies, settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent, createSignal } from "solid-js";
import { render } from "solid-js/web";
import type { Session_Serialize, SessionsView_Serialize, Workspace_Serialize } from "../../../../ainb-app/bindings/AppState";
import { FOLLOW_MS } from "../../src/composer.ts";
import { createNewAgentFlow } from "../../src/new_agent.ts";
import { shownTargetOf, worktreeTarget, type WorktreeTarget } from "../../src/worktree_target.ts";
import { createShellTabs, NO_SESSION } from "../../src/shell_tab.ts";
import { TabCreateMenu } from "../../src/tab_create_menu.tsx";
import type { Tab } from "../../src/tabs.ts";

const FIRST = "11111111-1111-4111-8111-111111111111";
const ADDED = "33333333-3333-4333-8333-333333333333";

function session(id: string): Session_Serialize {
  return { id, name: id, workspace_path: "/wt/app-feat", status: "Running", branch_name: "feat" } as unknown as Session_Serialize;
}

function frame(ids: string[], selected: string | null): SessionsView_Serialize {
  const workspace = { name: "app", path: "/repos/app", sessions: ids.map(session), shell_session: null };
  return { workspaces: [workspace as unknown as Workspace_Serialize], selected_session_id: selected } as unknown as SessionsView_Serialize;
}

const effects = { toasts: [] as string[], selected: [] as string[], restored: 0 };
const [listed, setListed] = createSignal<SessionsView_Serialize>(frame([FIRST], FIRST));
const [target, setTarget] = createSignal<WorktreeTarget | null>({ kind: "session", id: FIRST });
const clock = { now: 1_000 };

/** The window's wiring: one target for both items. */
function Harness() {
  const toast = (message: string) => effects.toasts.push(message);
  const agents = createNewAgentFlow({
    toast,
    sessions: listed,
    select: (sessionId) => effects.selected.push(sessionId),
    now: () => clock.now,
  });
  const shells = createShellTabs({ target, toast });
  return createComponent(TabCreateMenu, {
    get target() {
      return target();
    },
    mac: true,
    onNewTerminal: (at) => void shells.open(at),
    agents,
    restoreFocus: () => {
      effects.restored += 1;
    },
  });
}

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  document.body.innerHTML = "";
  hostReplies.clear();
  hostCalls.clear();
  effects.toasts = [];
  effects.selected = [];
  effects.restored = 0;
  setListed(frame([FIRST], FIRST));
  setTarget({ kind: "session", id: FIRST });
  clock.now = 1_000;
});

async function mount() {
  const container = document.createElement("div");
  document.body.appendChild(container);
  cleanup = render(Harness, container);
  await settle();
  return container;
}

const plus = (container: HTMLElement) => container.querySelector<HTMLButtonElement>("button.tab-new")!;
const menu = () => document.querySelector<HTMLElement>(".tab-create-menu");

async function open(container: HTMLElement) {
  plus(container).click();
  await settle();
  assert.ok(menu(), "the menu is open");
}

async function pick(container: HTMLElement, selector: string) {
  await open(container);
  const choice = menu()!.querySelector<HTMLButtonElement>(selector);
  assert.ok(choice, `the menu offers ${selector}`);
  choice.click();
  await settle();
}

const focused = () => (document.activeElement as HTMLElement).dataset;

test("one + holds New terminal, a separator, then the daemon's agents, as Orca's New tab menu", async () => {
  const container = await mount();
  assert.equal(container.querySelectorAll("button.tab-new").length, 1, "one + on the strip");
  assert.equal(plus(container).getAttribute("aria-label"), "New tab");
  assert.equal(menu(), null, "closed until pressed");
  await open(container);
  assert.equal(plus(container).getAttribute("aria-expanded"), "true");
  const rows = [...menu()!.children].map((row) =>
    row.getAttribute("role") === "separator" ? "---" : ((row as HTMLElement).dataset.item ?? (row as HTMLElement).dataset.agent),
  );
  assert.deepEqual(rows, ["terminal", "---", "claude", "codex", "gemini", "copilot", "antigravity"]);
  assert.match(menu()!.querySelector('[data-item="terminal"]')!.textContent ?? "", /New terminal.*⌘T/);
  menu()!.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  await settle();
  assert.equal(menu(), null, "Escape closes it");
  assert.ok(document.activeElement === plus(container), "and hands the keyboard back to the +");
  assert.equal(hostCalls.size, 0, "and sends nothing");
});

test("New terminal opens a shell on the menu's target, and hands the keyboard back", async () => {
  hostReplies.set("shell_open", "ainb-dsh-0123abcd");
  const container = await mount();
  await pick(container, '[data-item="terminal"]');
  assert.deepEqual(hostCalls.get("shell_open"), { target: { kind: "session", id: FIRST } });
  assert.equal(menu(), null);
  assert.equal(effects.restored, 1);
});

test("an agent is added on the target's ids, never a path, and its session is selected once listed", async () => {
  hostReplies.set("worktree_agent_add", {
    session_id: ADDED,
    tmux_session_name: "tmux_app-33333333",
    worktree_path: "/wt/app-feat",
    branch: "feat",
  });
  const container = await mount();
  await pick(container, '[data-agent="codex"]');
  assert.deepEqual(hostCalls.get("worktree_agent_add"), { args: { target: { kind: "session", id: FIRST }, agent: "codex" } });
  assert.equal(menu(), null, "the menu closes on a pick");
  assert.equal(effects.restored, 1, "the keyboard goes back to the shown tab");
  assert.deepEqual(effects.selected, [], "not before the session list carries the row");
  setListed(frame([FIRST, ADDED], FIRST));
  await settle();
  assert.deepEqual(effects.selected, [ADDED], "the new session is selected, as its tab is");
  assert.deepEqual(effects.toasts, []);
});

test("on a shell tab, both items name that tab and the host resolves its folder", async () => {
  hostReplies.set("worktree_agent_add", { session_id: ADDED, tmux_session_name: "t", worktree_path: "/wt", branch: "feat" });
  setTarget({ kind: "shell", key: "ainb-dsh-0123abcd" });
  const container = await mount();
  await pick(container, '[data-agent="claude"]');
  assert.deepEqual(hostCalls.get("worktree_agent_add"), { args: { target: { kind: "shell", key: "ainb-dsh-0123abcd" }, agent: "claude" } });
  await pick(container, '[data-item="terminal"]');
  assert.deepEqual(hostCalls.get("shell_open"), { target: { kind: "shell", key: "ainb-dsh-0123abcd" } });
});

test("a row that lands after the person moved on is not selected", async () => {
  hostReplies.set("worktree_agent_add", { session_id: ADDED, tmux_session_name: "t", worktree_path: "/wt", branch: "feat" });
  const container = await mount();
  await pick(container, '[data-agent="claude"]');
  clock.now += FOLLOW_MS + 1;
  setListed(frame([FIRST, ADDED], FIRST));
  await settle();
  assert.deepEqual(effects.selected, [], "past the follow deadline the selection stays put");
});

test("a refusal shows the host's sentence, the daemon's detail chosen by code", async () => {
  const detail = "The agent was started but has not reported back yet: `ainb run` is still running after 120s. Check the sidebar before adding another.";
  hostReplies.set("worktree_agent_add", new Error(detail));
  const container = await mount();
  await pick(container, '[data-agent="claude"]');
  assert.deepEqual(effects.toasts, [detail]);
  assert.deepEqual(effects.selected, []);
});

test("with nothing to open in, the + is off and says why", async () => {
  setTarget(null);
  const container = await mount();
  assert.equal(plus(container).disabled, true);
  assert.equal(plus(container).title, NO_SESSION);
});

test("agents are off while an add is on the host, so one press is one agent; New terminal is not", async () => {
  let answer: (value: unknown) => void = () => undefined;
  hostReplies.set("worktree_agent_add", new Promise((resolve) => (answer = resolve)));
  const container = await mount();
  await pick(container, '[data-agent="gemini"]');
  await open(container);
  assert.equal(menu()!.querySelector<HTMLButtonElement>('[data-agent="gemini"]')!.disabled, true, "off while the daemon works");
  assert.equal(menu()!.querySelector<HTMLButtonElement>('[data-item="terminal"]')!.disabled, false);
  answer({ session_id: ADDED, tmux_session_name: "t", worktree_path: "/wt", branch: "feat" });
  await settle();
  assert.equal(menu()!.querySelector<HTMLButtonElement>('[data-agent="gemini"]')!.disabled, false);
});

test("the menu closes when its target moves, so a pick never lands in another worktree", async () => {
  const container = await mount();
  await open(container);
  setTarget({ kind: "session", id: ADDED });
  await settle();
  assert.equal(menu(), null);
});

test("arrow keys walk the menu, wrapping, and Home and End jump to its ends", async () => {
  const container = await mount();
  await open(container);
  assert.equal(focused().item, "terminal", "the first item has focus");
  const key = (name: string) => menu()!.dispatchEvent(new window.KeyboardEvent("keydown", { key: name, bubbles: true }));
  key("ArrowDown");
  assert.equal(focused().agent, "claude", "the separator is skipped");
  key("ArrowUp");
  key("ArrowUp");
  assert.equal(focused().agent, "antigravity");
  key("Home");
  assert.equal(focused().item, "terminal");
  key("End");
  assert.equal(focused().agent, "antigravity");
});

test("the target is the shown tab's worktree, else the selection in view", () => {
  const tab = (key: string, target: Tab["target"]): Tab => ({ key, target, state: "attached" });
  const tabs = [
    tab("tmux_app", { kind: "session", id: "shown", tmux: "tmux_app" }),
    tab("ainb-dsh-1", { kind: "shell", tmux: "ainb-dsh-1", dir: "/wt/app" }),
    tab("bare", { kind: "tmux", tmux: "bare" }),
  ];
  const at = (active: string | null, showing = true) => worktreeTarget(shownTargetOf(showing, tabs, active), "selected");
  assert.deepEqual(at("tmux_app"), { kind: "session", id: "shown" }, "a session tab: its session");
  assert.deepEqual(at("ainb-dsh-1"), { kind: "shell", key: "ainb-dsh-1" }, "a shell tab: itself, the host knows its folder");
  assert.equal(at("bare"), null, "a bare tmux tab has no worktree");
  assert.deepEqual(at("tmux_app", false), { kind: "session", id: "selected" }, "no terminal shown: the sidebar's pick");
  assert.deepEqual(at(null), { kind: "session", id: "selected" }, "no tab: the sidebar's pick");
  assert.equal(worktreeTarget(undefined, null), null);
});
