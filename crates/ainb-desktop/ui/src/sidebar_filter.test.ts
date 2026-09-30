// The sidebar filter's match: which worktree cards a typed query keeps, and
// which project groups survive it. `test/dom/sidebar.test.ts` proves the
// field draws and clears it; this pins what "matches" means.

import assert from "node:assert/strict";
import { test } from "node:test";
import type { Session_Serialize } from "../../../ainb-app/bindings/AppState";
import { cardMatches, filterNeedle, filterProjectGroups, hidesProject, hidesSession } from "./sidebar_filter.ts";
import type { ProjectGroup, WorktreeCard } from "./sidebar_model.ts";

function session(id: string, over: Partial<Session_Serialize> = {}): Session_Serialize {
  return { id, name: id, display_name: null, ...over } as Session_Serialize;
}

function card(key: string, over: Partial<WorktreeCard> = {}): WorktreeCard {
  return { key, sessionId: key, title: key, branch: `ainb/${key}`, gitChanges: null, model: null, sessions: [session(key)], ...over };
}

function group(name: string, cards: WorktreeCard[]): ProjectGroup {
  return { name, path: `/${name}`, sessionCount: cards.length, cards };
}

test("an empty or whitespace-only query does not filter", () => {
  assert.equal(filterNeedle(""), null);
  assert.equal(filterNeedle("   \t"), null);
});

test("the needle is trimmed and lower-cased", () => {
  assert.equal(filterNeedle("  Fix-Login "), "fix-login");
});

test("a card matches on its title, branch, project name or any session's name, ignoring case", () => {
  const subject = card("wt", {
    title: "Login Page",
    branch: "feature/OAUTH",
    sessions: [session("s1", { name: "alpha" }), session("s2", { name: "beta" })],
  });
  for (const needle of ["login", "oauth", "hangar", "beta"]) {
    assert.equal(cardMatches(subject, "Hangar", needle), true, needle);
  }
  assert.equal(cardMatches(subject, "Hangar", "zeta"), false);
});

test("a card does not match on its agent type or model, which the filter does not index", () => {
  const subject = card("wt", { model: "opus", sessions: [session("s1", { agent_type: "Codex" })] });
  assert.equal(cardMatches(subject, "repo", "codex"), false);
  assert.equal(cardMatches(subject, "repo", "opus"), false);
});

test("a display name the card does not print is not matched", () => {
  // The card's title is its primary session's display name; another row's
  // display name is drawn nowhere, so it must not keep the card.
  const subject = card("wt", {
    title: "alpha",
    sessions: [session("alpha"), session("beta", { display_name: "Reviewer" })],
  });
  assert.equal(cardMatches(subject, "repo", "review"), false);
});

test("no query hands back the very same groups, so nothing reallocates", () => {
  const groups = [group("repo", [card("a")])];
  assert.equal(filterProjectGroups(groups, " "), groups);
});

test("a query keeps only matching cards and drops groups left with none", () => {
  const groups = [group("web", [card("login"), card("logout"), card("signup")]), group("api", [card("billing")])];
  const kept = filterProjectGroups(groups, "LOG");
  assert.deepEqual(
    kept.map((g) => [g.name, g.cards.map((c) => c.key)]),
    [["web", ["login", "logout"]]],
  );
});

test("a query naming a project keeps every card in it", () => {
  const groups = [group("web", [card("login"), card("signup")]), group("api", [card("billing")])];
  assert.deepEqual(
    filterProjectGroups(groups, "api").map((g) => g.cards.map((c) => c.key)),
    [["billing"]],
  );
});

test("a query nothing matches leaves no groups", () => {
  assert.deepEqual(filterProjectGroups([group("web", [card("login")])], "zzz"), []);
});

test("a kept group's count is the sessions in its kept cards, not the project's total", () => {
  const two = card("login", { sessions: [session("a"), session("b")] });
  const groups = [{ ...group("web", [two, card("signup")]), sessionCount: 3 }];
  assert.equal(filterProjectGroups(groups, "login")[0].sessionCount, 2);
});

test("hidesSession: true only for a session the frame has and the query hides", () => {
  const groups = [group("web", [card("login")]), group("api", [card("billing")])];
  assert.equal(hidesSession(groups, "login", "billing"), true);
  assert.equal(hidesSession(groups, "login", "login"), false);
  assert.equal(hidesSession(groups, "", "billing"), false, "no query hides nothing");
  assert.equal(hidesSession(groups, "login", "gone"), false, "a session not in the frame is not hidden by the filter");
});

test("hidesProject: true only for a drawn project the query drops whole", () => {
  const groups = [group("web", [card("login")]), group("api", [card("billing")])];
  assert.equal(hidesProject(groups, "login", "/api"), true);
  assert.equal(hidesProject(groups, "login", "/web"), false);
  assert.equal(hidesProject(groups, " ", "/api"), false);
  assert.equal(hidesProject(groups, "login", "/nowhere"), false);
});
