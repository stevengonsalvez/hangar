// The sidebar filter's match, pure: which worktree cards a typed query keeps.
// `sidebar_filter.tsx` is the field; `sidebar.tsx` draws what this returns.
//
// Orca's workspace search indexes a workspace's name, branch and project
// (`worktree-palette-document.ts`), and for its board only text printed on the
// card, so a card is never hidden by something the person cannot see. Here
// that is the card's title (its primary session's display name, else name),
// its branch, its project's name, and the names on its agent rows. Another
// row's display name is drawn nowhere, so it is not matched. Case is ignored and the query trimmed, as Orca's
// `repo-search.ts` does it.

import type { ProjectGroup, WorktreeCard } from "./sidebar_model.ts";

/** `query` lower-cased and trimmed, or `null` when it filters nothing: empty,
 * or only whitespace, which Orca also treats as no filter at all. */
export function filterNeedle(query: string): string | null {
  const needle = query.trim().toLowerCase();
  return needle === "" ? null : needle;
}

/** Whether `card`, in the project named `projectName`, holds `needle`, an
 * already-normalized `filterNeedle` result, in any text it prints. */
export function cardMatches(card: WorktreeCard, projectName: string, needle: string): boolean {
  const printed = [card.title, card.branch, projectName, ...card.sessions.map((session) => session.name)];
  return printed.some((text) => text.toLowerCase().includes(needle));
}

/**
 * `groups` narrowed to the cards `query` matches, and a group with none left
 * dropped whole, so the list never shows a project header over nothing. A
 * kept group counts the sessions it still draws, so its header never says 2
 * over one row. With
 * no query the very same array comes back, so an idle field costs nothing on
 * each frame.
 */
export function filterProjectGroups(groups: ProjectGroup[], query: string): ProjectGroup[] {
  const needle = filterNeedle(query);
  if (needle === null) return groups;
  const kept: ProjectGroup[] = [];
  for (const group of groups) {
    const cards = group.cards.filter((card) => cardMatches(card, group.name, needle));
    if (cards.length === 0) continue;
    const sessionCount = cards.reduce((sum, card) => sum + card.sessions.length, 0);
    kept.push({ ...group, sessionCount, cards });
  }
  return kept;
}

/** Whether `query` hides `sessionId`, a session one of `groups` holds. A
 * session the frame does not have is not the filter's doing, so `false`. */
export function hidesSession(groups: ProjectGroup[], query: string, sessionId: string): boolean {
  const holds = (from: ProjectGroup[]) =>
    from.some((group) => group.cards.some((card) => card.sessions.some((session) => session.id === sessionId)));
  return filterNeedle(query) !== null && holds(groups) && !holds(filterProjectGroups(groups, query));
}

/** Whether `query` drops the project at `path`, one of `groups`, whole. */
export function hidesProject(groups: ProjectGroup[], query: string, path: string): boolean {
  const holds = (from: ProjectGroup[]) => from.some((group) => group.path === path);
  return filterNeedle(query) !== null && holds(groups) && !holds(filterProjectGroups(groups, query));
}
