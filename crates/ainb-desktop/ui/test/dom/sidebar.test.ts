// The sidebar mounted: what a person actually sees for a frame, and what its
// one control does. `sidebar_model.test.ts` holds the grouping projection;
// this mounts the component the window mounts, so a card that draws none of
// it, a row whose click sends nothing, or a collapse that does not survive a
// relaunch, fails here rather than passing on a projection nobody renders.

import { settle } from "./window.ts";

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createComponent, createSignal } from "solid-js";
import { render } from "solid-js/web";
import type {
  AgentCardFrame,
  FleetView_Serialize,
  Session_Serialize,
  SessionsView_Serialize,
} from "../../../../ainb-app/bindings/AppState";
import type { PendingWorktree } from "../../src/composer.ts";
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

function frame(over: Partial<SessionsView_Serialize> = {}): SessionsView_Serialize {
  return {
    workspaces: [
      {
        name: "repo",
        path: "/repo",
        // "claude-1" and "shell-1" share a worktree: one card, two agent rows.
        sessions: [
          session("claude-1", "/repo/wt-a"),
          session("shell-1", "/repo/wt-a", { agent_type: "Shell", name: "shell-1" }),
        ],
        shell_session: null,
      },
    ],
    selected_workspace_index: 0,
    selected_session_id: null,
    shell_selected: false,
    selected_sessions: [],
    expand_all_workspaces: false,
    session_filter: "active_only",
    attached_session_id: null,
    favorite_workspace_paths: [],
    ...over,
  };
}

let cleanup: (() => void) | undefined;
afterEach(() => {
  cleanup?.();
  cleanup = undefined;
  document.body.innerHTML = "";
  window.localStorage.clear();
});

/** Mount the sidebar the way `main.tsx` does: a fresh frame per read. */
async function open(
  first: SessionsView_Serialize = frame(),
  onOpen: (id: string) => void = () => undefined,
  pending: PendingWorktree | null = null,
  status: { cards?: readonly AgentCardFrame[]; fleetMetadata?: FleetView_Serialize["fleet_metadata"] } = {},
) {
  const [held, setHeld] = createSignal(first);
  const [held_pending, setPending] = createSignal(pending);
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
        get pending() {
          return held_pending();
        },
        cards: status.cards,
        fleetMetadata: status.fleetMetadata,
        onOpen,
        onNew() {
          // The composer itself is `main.tsx`'s own test; this only proves
          // the button is wired to whatever the caller passed.
        },
        ref() {
          // The sidebar element itself, unused here: `main.tsx` uses it for
          // Esc Esc, which is that module's own test.
        },
      }),
    container,
  );
  await settle();
  return { setHeld, setPending };
}

test("two sessions in one worktree render one card with two agent rows", async () => {
  await open();
  const cards = [...document.querySelectorAll("li.worktree-card")];
  assert.equal(cards.length, 1, "one worktree, one card");
  const rows = [...cards[0].querySelectorAll(".session-row")];
  assert.deepEqual(
    rows.map((row) => row.getAttribute("data-session")),
    ["claude-1", "shell-1"],
  );
});

test("a click on a session row opens that session, whichever card it is in", async () => {
  let opened: string | null = null;
  await open(frame(), (id) => {
    opened = id;
  });
  document.querySelector<HTMLButtonElement>('.session-row[data-session="shell-1"]')?.click();
  await settle();
  assert.equal(opened, "shell-1");
});

test("the selected session's row carries aria-current; the others do not", async () => {
  await open(frame({ selected_session_id: "shell-1" }));
  assert.equal(document.querySelector('.session-row[data-session="shell-1"]')?.getAttribute("aria-current"), "true");
  assert.equal(document.querySelector('.session-row[data-session="claude-1"]')?.getAttribute("aria-current"), null);
});

test("a row's dot reads the same UiStatus its card would (P4)", async () => {
  const cards: AgentCardFrame[] = [
    {
      session_key: "claude:p-1",
      state: "waiting",
      wait_kind: "approval",
      provider: "claude",
      lifecycle: "RUNNING",
      transport_health: "HEALTHY",
      has_open_request: true,
      turn_complete: false,
      tier: "hook",
      evidence_observed_at: 1,
    } as AgentCardFrame,
  ];
  const fleetMetadata = { "claude-1": { provider_session_id: "p-1" } } as unknown as FleetView_Serialize["fleet_metadata"];
  await open(frame(), undefined, null, { cards, fleetMetadata });
  const dot = document.querySelector('.session-row[data-session="claude-1"] .ring');
  assert.equal(dot?.getAttribute("data-status"), "needs-approve");
});

test("a row with no matching card falls back to its own ring and lifecycle", async () => {
  await open();
  const dot = document.querySelector('.session-row[data-session="claude-1"] .ring');
  // The fixture session is `Running` (its tmux session is alive) with no
  // attention chips and no card: nothing says the agent is working, so it
  // reads unverifiable, never a spinner.
  assert.equal(dot?.getAttribute("data-status"), "unverifiable");
});

test("a pending create draws a working card ahead of its project's real cards", async () => {
  const { setPending } = await open(frame(), () => undefined, { projectPath: "/repo", name: "fix login" });
  const cards = [...document.querySelectorAll("li.worktree-card")];
  assert.equal(cards.length, 2, "the pending card plus the one real card");
  assert.equal(cards[0].getAttribute("data-pending"), "true");
  assert.equal(cards[0].textContent?.trim(), "fix login");
  assert.ok(cards[0].querySelector(".spinner"), "the pending card shows a working spinner");
  assert.equal(cards[1].hasAttribute("data-pending"), false, "the real card is not marked pending");

  // The request settles: the card leaves, the real cards are unaffected.
  setPending(null);
  await settle();
  assert.equal(document.querySelectorAll("li.worktree-card").length, 1);
});

test("a pending create for another project draws no card here", async () => {
  await open(frame(), () => undefined, { projectPath: "/somewhere-else", name: "fix login" });
  assert.equal(document.querySelector('li.worktree-card[data-pending="true"]'), null);
});

test("the sidebar's new-worktree button calls onNew", async () => {
  let opened = 0;
  const [held] = createSignal(frame());
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
        onOpen: () => undefined,
        onNew: () => {
          opened += 1;
        },
        ref() {
          // Unused here.
        },
      }),
    container,
  );
  await settle();
  document.querySelector<HTMLButtonElement>(".sidebar-new")?.click();
  assert.equal(opened, 1);
});

test("collapsing a project persists across a relaunch", async () => {
  await open();
  const details = document.querySelector<HTMLDetailsElement>('details.workspace[data-workspace="repo"]');
  assert.equal(details?.open, true, "open by default");

  // A person collapses it: the native disclosure, not a synthetic prop write.
  document.querySelector<HTMLElement>("summary.workspace-name")?.click();
  await settle();
  assert.equal(details?.open, false, "the click closed it");

  // The relaunch: a fresh mount, over the same localStorage.
  cleanup?.();
  document.body.innerHTML = "";
  await open();
  const reopened = document.querySelector<HTMLDetailsElement>('details.workspace[data-workspace="repo"]');
  assert.equal(reopened?.open, false, "the collapse survived the relaunch");
});

/**
 * Every frame rebuilds the rows; the sidebar draws them by key, so a frame
 * that changes ANOTHER row patches in place and the focused row keeps the
 * keyboard. Drawn by object, the whole list remounted and focus fell to
 * <body> mid-typing (#1267's failure, in the sidebar).
 */
test("a focused row keeps focus when a frame changes another row", async () => {
  const two = (claudeChanges: number) =>
    frame({
      workspaces: [
        {
          name: "repo",
          path: "/repo",
          sessions: [
            session("claude-1", "/repo/wt-a", { git_changes: { added: claudeChanges, modified: 0, deleted: 0 } }),
            session("codex-1", "/repo/wt-b", { agent_type: "Codex", name: "codex-1" }),
          ],
          shell_session: null,
        },
      ],
    });
  const { setHeld } = await open(two(0));
  const row = document.querySelector<HTMLButtonElement>('.session-row[data-session="codex-1"]');
  row?.focus();
  assert.equal(document.activeElement, row, "the row took focus");

  // A fresh frame object, as the host sends, with only claude-1's counts moved.
  setHeld(two(3));
  await settle();

  assert.equal(
    document.activeElement,
    row,
    "the same node still holds focus, not <body>"
  );
  assert.equal(document.querySelector('.session-row[data-session="codex-1"]'), row, "the row was patched, not remounted");
  assert.match(document.querySelector(".git-counts")?.textContent ?? "", /\+3/, "the other card did update");
});

test("collapse is remembered by project path, so two repos with one name stay apart", async () => {
  const twins = frame({
    workspaces: [
      { name: "app", path: "/work/app", sessions: [session("a-1", "/work/app/wt")], shell_session: null },
      { name: "app", path: "/personal/app", sessions: [session("b-1", "/personal/app/wt")], shell_session: null },
    ],
  });
  await open(twins);
  const [first, second] = [...document.querySelectorAll<HTMLDetailsElement>("details.workspace")];
  first.querySelector<HTMLElement>("summary")?.click();
  await settle();
  assert.equal(first.open, false);
  assert.equal(second.open, true, "the other 'app' stays open");
});
