import { render } from "solid-js/web";
import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { FrameBatch_Serialize, HostId } from "../../../ainb-app/bindings/AppState";
import { ackTurn, pruneAcks, readAcks, rowAckKey, writeAcks, type AckMap, type AckStorage } from "./acks.ts";
import { createFrameStore } from "./store.ts";
import { Stats } from "./stats.tsx";
import { UsageSegment } from "./usage_segment.tsx";
import {
  configRevision,
  shellAgentStatus,
  shellConfig,
  shellFleet,
  shellGitView,
  shellHangar,
  shellInbox,
  shellLabels,
  shellSessions,
  shellUsage,
  SUBSCRIBED,
} from "./subscription.ts";
import { allSessions, label, ringFor } from "./sessions.ts";
import { ROOT_SELECTORS } from "./selectors.ts";
import { AcpCard } from "./acp.tsx";
import { transcriptIntent, transcriptView } from "./acp.ts";
import { AnswerSlot } from "./answer.tsx";
import { phaseOf, questionFor, questionOver, type Refusal, sendInOrder } from "./answer.ts";
import { newNotices, noticeKey } from "./notices.ts";
import { terminal as updateDone, updateLine, type UpdatePhase } from "./update.ts";
import { Board } from "./board.tsx";
import { agentStateCounts, boardColumns, countIn, nextNeedsYou, sameColumns, showIntents } from "./board.ts";
import { readCollapsed, stepWorktree } from "./sidebar_model.ts";
import { CLOSE_INBOX, OPEN_INBOX, inboxCounts } from "./inbox.ts";
import { Inbox } from "./inbox.tsx";
import { SURFACES } from "./surfaces.ts";
import { Commits } from "./commits.tsx";
import { Review } from "./review.tsx";
import { Palette } from "./palette.tsx";
import { emptyPaneView } from "./pane_empty.ts";
import { EmptyPane } from "./pane_empty.tsx";
import { createComposerFlow } from "./composer.ts";
import { createNewAgentFlow } from "./new_agent.ts";
import { TabCreateMenu } from "./tab_create_menu.tsx";
import { cardForSession, statusForTarget } from "./status.ts";
import {
  activateTab,
  focusGroup,
  groups,
  initialLayout,
  readStored,
  splitGroup,
  writeLayout,
  type GroupId,
  type Layout,
} from "./layout.ts";
import { beginRestore, followHost, rebuild, restoreDone, type Restore } from "./panes.ts";
import { Panes } from "./panes.tsx";
import { createShellTabs, reattach, shellTitle } from "./shell_tab.ts";
import {
  closedFrom,
  PLACE_MS,
  placeReopened,
  popReopenable,
  pushClosed,
  switcherOrder,
  type ClosedTab,
  type Placing,
} from "./recent_tabs.ts";
import { createSwitcher } from "./recent_tabs.tsx";
import { shownTargetOf, worktreeTarget } from "./worktree_target.ts";
import { Composer } from "./composer.tsx";
import { createDeleteFlow } from "./delete_dialog.ts";
import { DeleteDialog } from "./delete_dialog.tsx";
import { Sidebar } from "./sidebar.tsx";
import { runRowPick } from "./row_menu.ts";
import { Titlebar } from "./titlebar.tsx";
import { Statusbar } from "./statusbar.tsx";
import { SettingsPage } from "./settings.tsx";
import { CLOSE_SETTINGS, OPEN_SETTINGS } from "./settings.ts";
import { banner as sidecarBanner, retryable, type SidecarState } from "./sidecar.ts";
import type { SetupView, SetupWrite } from "../../bindings/Desktop.ts";
import {
  openRowIntent,
  rowOf,
  selectIntentFor,
  selectRowIntent,
  shownSessionOf,
  stepTab,
  visited,
  modalBlocks,
  shellKeydown,
  terminalMayTakeFocus,
  type Accelerator,
  type RendererIntent,
  type RowId,
  type Tab,
  type TabsView,
} from "./tabs.ts";
import { TerminalView } from "./terminal.tsx";
import "@fontsource-variable/geist";
import "@fontsource-variable/geist-mono";
import "./theme/tokens.css";
import "./shell.css";
import { startTheme } from "./theme/theme.ts";

/** How long batches gather before one drain applies them all. */
const DRAIN_MS = 16;

/** The most characters a toast draws: the host's own cut
 * (`intent::MAX_TOAST_CHARS`). A sidebar label's 80 would drop a refusal's
 * detail, the part that says what to do. */
const TOAST_CHARS = 300;

/** `text` as a toast draws it: control and format characters removed, as a
 * label's are, and cut to `TOAST_CHARS`. */
function toastLine(text: string): string {
  return Array.from(text.replace(/[\p{Cc}\p{Cf}]/gu, ""))
    .slice(0, TOAST_CHARS)
    .join("");
}

/** How long a toast stays up. */
const TOAST_MS = 5000;

const MAC = navigator.userAgent.includes("Mac");

/**
 * `window.localStorage`, or `undefined` when it is missing, throws (a
 * private window, blocked site data), or this module evaluates with no
 * `window` at all: the same guard `sidebar.tsx` and `theme.ts` use.
 */
function safeStorage(): AckStorage | undefined {
  try {
    return window.localStorage;
  } catch {
    return undefined;
  }
}

function Shell() {
  const store = createFrameStore(SUBSCRIBED);
  const [sidecar, setSidecar] = createSignal<SidecarState>({ state: "starting" });
  const [log, setLog] = createSignal<string | null>(null);

  // The host the channel is connected to: the `subscribe` answer, then every
  // `host` event. The host sends that event before it re-pins its frames to a
  // new id (#1066), so every drain after it is applied as the new host's.
  const [peer, setPeer] = createSignal<HostId>();

  // Registered synchronously: an `onCleanup` after an `await` has left the
  // owner and never runs.
  // The updater's framed state: one line while an update is in flight.
  const [updatePhase, setUpdatePhase] = createSignal<UpdatePhase | null>(null);
  const listeners = [
    listen<SidecarState>("sidecar", (event) => setSidecar(event.payload)),
    listen<TabsView>("terminal_tabs", (event) => showTabs(event.payload)),
    listen<string>("toast", (event) => toast(event.payload)),
    listen<UpdatePhase>("update", (event) => {
      setUpdatePhase(event.payload);
      if (updateDone(event.payload)) setTimeout(() => setUpdatePhase(null), TOAST_MS);
    }),
    listen<HostId>("host", (event) => {
      // The host re-pinned its frames to a new id (#1066). What the old id
      // left in the store is never framed again: drop it, so it neither shows
      // nor holds a MAX_HOSTS slot.
      const stale = peer();
      setPeer(event.payload);
      if (stale !== undefined && stale !== event.payload) store.evictHost(stale);
    }),
  ];
  onCleanup(() => listeners.forEach((unlisten) => void unlisten.then((stop) => stop())));

  // Terminal tabs: the strip is the Rust side's; where each tab sits, in
  // which group of the split panes, and which tab each group shows is ours
  // (`layout.ts`), kept in this window's storage across reloads.
  const [tabs, setTabs] = createSignal<Tab[]>([]);
  const [layout, setLayout] = createSignal<Layout>(initialLayout([]));
  // Where the window is in bringing the stored layout back. The host
  // restores no tabs: after a relaunch its strip is empty, then gains a tab
  // at a time as a person opens them. So the stored layout is read on the
  // first strip with a tab, and rebuilt from on every strip (never written)
  // until every stored tab is open again, a person reshapes the panes, or
  // `RESTORE_MS` passes. Only then does the window follow the host and store
  // what it shows; following earlier would drop each stored tab not open
  // yet, and store that over the layout being restored.
  let phase: "waiting" | Restore | "following" = "waiting";
  // The tab last activated during a restore: kept shown across rebuilds.
  let restoreShown: string | null = null;
  /** The shown tab of the focused group: the terminal with the keyboard,
   * the one the sidebar and the answer banner follow. */
  const active = () => {
    const current = layout();
    return groups(current).find((group) => group.id === current.focused)?.active ?? null;
  };
  /** Every tab in the order the panes draw them: pane by pane in reading
   * order, each strip left to right. What the tab chords count and step
   * through, so Cmd+2 is the second tab a person sees. */
  const inView = () => {
    const byKey = new Map(tabs().map((tab) => [tab.key, tab]));
    return groups(layout()).flatMap((group) => group.tabs.flatMap((key) => byKey.get(key) ?? []));
  };
  /** Take `next` as the layout, and store it once the window follows the
   * host (`phase`); the same layout is a no-op. */
  const commitLayout = (next: Layout) => {
    if (next === layout()) return;
    setLayout(next);
    if (phase === "following") writeLayout(safeStorage(), next);
  };
  // Tab keys in the order they were shown, most recent last: which tab a
  // group shows when its shown one closes. Read only then, so not a signal.
  let recent: string[] = [];
  // The tabs a person closed, newest first (`pushClosed`), and the ones on
  // their way back, by the key they come back as, still to be put where
  // they were (`placeReopened`).
  let closedTabs: ClosedTab[] = [];
  const placing = new Map<string, Placing>();
  /** `next` with the reopened tabs it holds put back (`placeReopened`). */
  const placeBack = (next: Layout) => {
    const back = placeReopened(next, placing, Date.now());
    back.done.forEach((key) => placing.delete(key));
    return back.layout;
  };
  // The session the last worktree step opened, until its tab is shown: the
  // next step goes on from it, not again from the tab still on screen.
  let steppedTo: string | null = null;
  // The board is the window's landing surface: what every agent is doing, and
  // what is waiting on a human. A terminal takes the work area while it is
  // chosen, and the board is one click back.
  // What holds the work area. One choice rather than a flag each: two booleans
  // for one pane is three ways to be wrong and a fourth that draws nothing.
  // The transcript card is not in here; it stands in a session's place and
  // closes back to whatever was chosen.
  const [pane, setPane] = createSignal<"board" | "review" | "commits" | "stats" | "terminal">("board");
  // The ACP session whose transcript card holds the work area, if any. It has
  // no tmux pane, so the card stands where its terminal would.
  const [transcriptKey, setTranscriptKey] = createSignal<string | null>(null);
  /**
   * Whether `which` holds the work area: the transcript card takes it first,
   * and the settings page and the inbox page (the reducer on its Config or
   * Inbox screen) take it over every pane.
   */
  const showing = (which: "board" | "review" | "commits" | "stats" | "terminal") =>
    transcriptKey() === null && !settings() && !inboxOpen() && pane() === which;
  // The settings page: the config section as a form, the daemons panel and
  // the Setup panel (D3d). Whether it is open is the reducer's: the page shows
  // while `shell.current_screen` is the Config screen. Opening walks the
  // reducer there, where the form's row edits are in context; closing walks
  // it back to the session list the sidebar is. The window keeps no copy.
  const [setup, setSetup] = createSignal<SetupView | null>(null);
  const focusers = new Map<string, () => void>();
  let sidebar: HTMLElement | undefined;
  // Whether the sidebar is shown: Cmd+B, Ctrl+Shift+B off macOS. This
  // window's own, not saved; every launch opens with it shown.
  const [sidebarShown, setSidebarShown] = createSignal(true);

  /**
   * Give the keyboard to `key`'s terminal, on the next frame so a tab that
   * was just listed has mounted, when `terminalMayTakeFocus` allows it: never
   * under the open palette, and not for the host's own answer while a text
   * field such as the answer banner's composer has the keyboard. The host
   * answers a tab open on its own schedule, so its focus can land after the
   * chord that opened the palette or the click that put the cursor in the
   * composer, and the keystrokes meant for that field, Escape among them,
   * would go to the agent's pane (#47). A person's own tab chord or click
   * moves the keyboard as asked. The palette gives the keyboard back to the
   * active tab when it closes.
   */
  const focusTab = (key: string, byHost: boolean) =>
    requestAnimationFrame(() => {
      if (terminalMayTakeFocus({ palette: palette(), composer: modalOpen(), byHost, active: document.activeElement })) {
        focusers.get(key)?.();
      }
    });
  // Done-until-ack, per viewer: one map, read once from this window's own
  // storage, shared by the sidebar row, the tab chip and the board card, so
  // the three surfaces can never disagree about which turn was opened.
  const [acks, setAcks] = createSignal<AckMap>(readAcks(safeStorage()));
  const ackSession = (sessionKey: string, turnMarker: number) => {
    setAcks((current) => {
      const next = ackTurn(current, sessionKey, turnMarker);
      if (next !== current) writeAcks(safeStorage(), next);
      return next;
    });
  };
  // Prune acks to what this window can still see: the cards, and the rows
  // whose Done is only a chip. A chip that clears drops its row ack, so its
  // next Done shows again, and storage never grows with every session ever.
  createEffect(() => {
    // Not before both frames have landed: an empty first read would prune
    // every ack this viewer stored last time.
    const view = agentStatus()?.view;
    if (view === null || view === undefined || sessions() === undefined) return;
    const live = new Set<string>(view.cards.map((card) => card.session_key));
    for (const row of allSessions(sessions())) {
      if (ringFor(row) === "Done") live.add(rowAckKey(row.id));
    }
    setAcks((current) => {
      const next = pruneAcks(current, live);
      if (next !== current) writeAcks(safeStorage(), next);
      return next;
    });
  });
  /** `key`'s own card, when it names a session with one: the join `status.ts`
   * uses, so opening a tab acks the exact turn its glyph shows. */
  const cardForTabKey = (key: string) => {
    const tab = tabs().find((candidate) => candidate.key === key);
    const target = tab?.target;
    if (target === undefined || target.kind !== "session") return undefined;
    const session = allSessions(sessions()).find((row) => row.id === target.id);
    return session === undefined
      ? undefined
      : cardForSession(session, agentStatus()?.view?.cards ?? [], fleet()?.fleet_metadata);
  };

  /**
   * Show `key`'s terminal. `byHost` says who asked: the host, answering a
   * tab open on its own schedule, or a person, by a chord or a click. Every
   * caller says which, since the difference decides whether the terminal may
   * take the keyboard from a text field (`terminalMayTakeFocus`).
   */
  const activate = (key: string | null, byHost: boolean, first: RendererIntent[] = []) => {
    steppedTo = null;
    if (key !== null) {
      // Shown in its own group, and that group focused: the host's
      // `TabsView.focus` lands here as a person's click does.
      commitLayout(activateTab(layout(), key));
      if (typeof phase === "object") restoreShown = key;
      recent = visited(recent, key);
      // The session list follows the shown terminal, whoever showed it, so
      // the sidebar row and the answer banner are that session's.
      const select = selectIntentFor(tabs(), key);
      // Back to the session list first, as Orca leaves any page for the
      // terminal before it activates a worktree: on Settings or the Inbox the
      // reducer is on a screen whose gate refuses a session-list row. The host
      // decides by the reducer's own screen (#121), which a frame can trail,
      // and `first` and the row go only once it is back on the list.
      void invoke("answer_home").then(() => run([...first, ...(select === null ? [] : [select])]));
      setPane("terminal");
      closeTranscript();
      focusTab(key, byHost);
      // Opening a session acks the Done it shows, on every surface: its
      // card's turn, or, for a Done that is only a chip, the row itself.
      const card = cardForTabKey(key);
      if (card !== undefined) ackSession(card.session_key, card.evidence_observed_at);
      else {
        const target = tabs().find((candidate) => candidate.key === key)?.target;
        // Only a row that shows Done has a Done to ack: acking any other row
        // would pre-ack the next Done it shows before anyone saw it.
        const row =
          target?.kind === "session"
            ? allSessions(sessions()).find((session) => session.id === target.id)
            : undefined;
        if (row !== undefined && ringFor(row) === "Done") ackSession(rowAckKey(row.id), 0);
      }
    }
  };
  /**
   * How many tab-strip answers the host has given so far, and which tab the
   * last one focused, on the strip as `data-host-answers` and
   * `data-host-focus`: the host answers a tab open on its own schedule, and
   * a driven run that must act after that answer (not before, not a guessed
   * second later, not on a strip tidy-up that focused nothing) has nothing
   * else to read it from.
   */
  const [hostAnswers, setHostAnswers] = createSignal(0);
  const [hostFocus, setHostFocus] = createSignal("");
  const showTabs = (view: TabsView) => {
    setHostAnswers((n) => n + 1);
    setHostFocus(view.focus ?? "");
    const keys = view.tabs.map((tab) => tab.key);
    const shownBefore = active();
    setTabs(view.tabs);
    recent = recent.filter((key) => keys.includes(key));
    for (const key of focusers.keys()) {
      if (!keys.includes(key)) focusers.delete(key);
    }
    // Followed (`followHost`), which returns the layout itself when nothing
    // changed, so a strip that only restates the tabs costs nothing; or,
    // while the stored layout comes back, rebuilt from it (`phase`).
    if (phase === "following") {
      commitLayout(placeBack(followHost(layout(), keys, recent)));
    } else if (keys.length > 0) {
      const restore = phase === "waiting" ? beginRestore(readStored(safeStorage()), Date.now()) : phase;
      phase = restoreDone(restore, keys, Date.now()) ? "following" : restore;
      commitLayout(rebuild(restore, keys, restoreShown));
      // Stored on the strip that ends the restore, even when the rebuild is
      // the layout already shown and so wrote nothing.
      if (phase === "following") writeLayout(safeStorage(), layout());
    }
    if (view.focus !== null) activate(view.focus, true);
    else if (active() !== shownBefore) {
      // The shown tab closed or ended (its tmux session died, say), and its
      // group shows the one Orca would show next, or closed into its
      // neighbour: point there WITHOUT leaving the board. `activate` means a
      // person chose a terminal; this is the strip tidying up after itself.
      const next = active();
      if (next !== null) recent = visited(recent, next);
      if (next !== null && pane() === "terminal") {
        // The sidebar follows the terminal now shown, as `activate` has it
        // follow every other: left on the closed tab's session, the answer
        // banner's scope (`questionOver`) would hide this one's question.
        // Not under Settings or the Inbox: no terminal is shown there, and
        // the reducer, on that page, refuses a session-list row. The host
        // decides by its own screen, which a frame can trail (as `activate`'s
        // `answer_home` allows for), so a refusal here is the strip tidying
        // up on a page the frame has not shown yet: nobody asked, no toast.
        if (!settings() && !inboxOpen()) {
          const select = selectIntentFor(view.tabs, next);
          if (select !== null) void invoke<Refusal | null>("dispatch", { intent: select });
        }
        focusTab(next, true);
      }
    }
  };
  // A refused intent comes back with the row and the reason: say so, or a
  // key the window may not use (onboarding installs, a commit) looks dead.
  const report = (refusal: Refusal | null) => {
    if (refusal) toast(`${refusal.command} is not run from the window: ${refusal.reason}`);
  };
  const dispatch = (intent: RendererIntent) => void invoke<Refusal | null>("dispatch", { intent }).then(report);
  const openTranscript = (sessionKey: string) => {
    dispatch(transcriptIntent(sessionKey));
    setTranscriptKey(sessionKey);
    setPane("terminal");
  };
  /** Close the card; the host drops the transcript and frames the default. */
  function closeTranscript() {
    if (transcriptKey() === null) return;
    dispatch(transcriptIntent(null));
    setTranscriptKey(null);
  }
  /**
   * Send `intents` in order, each one applied before the next is sent: the
   * host applies a dispatch before the command returns, so awaiting each keeps
   * a cursor move ahead of the Enter that reads it. A refusal on any of them
   * is reported the same way.
   */
  // Stops at the first refusal: a pick whose cursor move was refused must not
  // go on to send Enter on whatever option the reducer is pointing at.
  const run = async (intents: RendererIntent[]): Promise<Refusal | null> => {
    const refusal = await sendInOrder(intents, (intent) => invoke<Refusal | null>("dispatch", { intent }));
    report(refusal);
    return refusal;
  };
  /** Select a session-list row and attach it, so the reducer marks it attached. */
  const openRow = (row: RowId) => dispatch(openRowIntent(row));
  const refreshSetup = () => void invoke<SetupView>("setup_status").then(setSetup);
  const openSettings = () => {
    closeTranscript();
    void run(OPEN_SETTINGS);
    refreshSetup();
  };
  const closeSettings = () => {
    if (!settings()) return;
    void run(CLOSE_SETTINGS);
  };
  // The inbox page, the same way: open walks the reducer to its Inbox screen,
  // where the sweep is active; close walks it back to the session list.
  const openInbox = () => {
    closeTranscript();
    void run(OPEN_INBOX);
  };
  const closeInbox = () => {
    if (!inboxOpen()) return;
    void run(CLOSE_INBOX);
  };
  /**
   * The answer banner's sends. Its rows are the session list's, and the
   * banner is drawn over every page: the host is asked to put the reducer on
   * the session list first, by the reducer's own screen (#121), and then the
   * rows go as before.
   */
  const answer = async (intents: RendererIntent[]): Promise<Refusal | null> => {
    if (intents.length === 0) return null;
    await invoke("answer_home");
    return run(intents);
  };
  /** The shell confirms in its own dialog, runs the write, and toasts the outcome. */
  const setupWrite = (write: SetupWrite) =>
    void invoke<boolean>("setup_write", { write }).then((ran) => {
      if (ran) refreshSetup();
    });

  // The palette is mounted only while it is open: each opening lists the
  // commands afresh, with the host's answer for which of them run now.
  const [palette, setPalette] = createSignal(false);
  /** Give the keyboard back to the shown tab's terminal, else the sidebar. */
  const focusShown = () => {
    const key = active();
    if (key !== null) focusers.get(key)?.();
    else sidebar?.focus();
  };
  const closePalette = () => {
    setPalette(false);
    focusShown();
  };
  const choose = (tab: Tab) => {
    // A shell has no row to re-attach through: the host re-attaches it.
    const viaRow = tab.state === "detached" && tab.target.kind !== "shell";
    if (tab.state === "detached" && !viaRow) reattach(tab);
    activate(tab.key, false, viaRow ? [openRowIntent(rowOf(tab.target))] : []);
  };
  /** Close `tab`: its terminal goes, and the host's next strip drops it
   * from its group, closing the group if it was the last. A shell's tab
   * also ends its shell (`shell_tab.ts`). */
  const closeTab = (tab: Tab) => {
    closedTabs = pushClosed(closedTabs, closedFrom(layout(), tab));
    shellTabs.close(tab);
  };
  /** Mod+Shift+T: the newest closed tab that can come back does, where it
   * was (`popReopenable`); the host's tab strip brings it. */
  const reopenClosed = () => {
    const { found, rest } = popReopenable(closedTabs, tabs(), sessions());
    closedTabs = rest;
    if (found === null) return;
    const { closed, reopen } = found;
    /** Put the tab `key` back where `closed` was once the strip lists it,
     * within `PLACE_MS`: a tab that never comes, or comes much later by some
     * other open, is not moved. */
    const expectBack = (key: string) => placing.set(key, { closed, until: Date.now() + PLACE_MS });
    if (reopen.kind === "row") {
      expectBack(closed.key);
      void answer([openRowIntent(reopen.row)]).then(
        (refusal) => {
          if (refusal !== null) placing.delete(closed.key);
        },
        () => placing.delete(closed.key),
      );
      return;
    }
    void shellTabs.open(reopen.target).then((key) => {
      if (key === null) return;
      expectBack(key);
      // The strip with the new shell can land before this answer does.
      commitLayout(placeBack(layout()));
    });
  };
  /** Ctrl+Tab: the focused pane's tabs, most recently shown first. */
  const switcher = createSwitcher({
    candidates: () => {
      const current = layout();
      const group = groups(current).find((one) => one.id === current.focused);
      return group === undefined ? null : { keys: switcherOrder(group.tabs, recent, group.active), shown: group.active };
    },
    label: (key) => {
      const tab = tabs().find((candidate) => candidate.key === key);
      return tab === undefined ? key : title(tab);
    },
    // Not behind a modal or the palette, nor off the terminals: a chord
    // pressed while Ctrl was held can have opened one.
    commit: (key) => {
      if (modalOpen() || palette() || !showing("terminal")) return;
      const tab = tabs().find((candidate) => candidate.key === key);
      if (tab !== undefined) choose(tab);
    },
  });
  /** The Terminals entry: back to the panes, on the tab they show. */
  const showTerminals = () => {
    const tab = tabs().find((candidate) => candidate.key === active());
    if (tab !== undefined) {
      choose(tab);
      return;
    }
    void invoke("answer_home");
    closeTranscript();
    setPane("terminal");
  };
  /**
   * Take a layout the panes made (a split, a move, a resize). When it moved
   * the keyboard, to another group or another tab, that tab is activated as
   * a click on it would be, so the sidebar and the answer banner follow it.
   */
  const applyLayout = (next: Layout) => {
    const before = layout();
    const shownBefore = active();
    // A person reshaping the panes ends a restore: what they made is kept.
    phase = "following";
    commitLayout(next);
    const shown = active();
    if (shown !== null && (next.focused !== before.focused || shown !== shownBefore)) activate(shown, false);
  };
  /** A press or the keyboard went into group `id`: it takes the focus, on
   * the tab it shows. */
  const focusPaneGroup = (id: GroupId) => {
    const current = layout();
    if (current.focused === id) return;
    const shown = groups(current).find((group) => group.id === id)?.active ?? null;
    if (shown !== null) activate(shown, false);
    else commitLayout(focusGroup(current, id));
  };
  /** "Close split pane": every tab of the group closes, as Orca's does
   * (`orca:src/renderer/src/components/tab-group/useTabGroupCloseScopeCommands.ts:24-34`).
   * The terminals are the host's to end; its next strip, without them,
   * collapses the group. */
  const closePaneGroup = (id: GroupId) => {
    const group = groups(layout()).find((candidate) => candidate.id === id);
    for (const key of group?.tabs ?? []) {
      const tab = tabs().find((candidate) => candidate.key === key);
      if (tab !== undefined) closeTab(tab);
    }
  };
  /** Open the palette, or close it the way Esc does: the titlebar's search
   * button and the Cmd+J (Ctrl+Shift+J) accelerator both send exactly this, so
   * there is one place that decides which way a press or a click goes. */
  const togglePalette = () => {
    if (palette()) closePalette();
    else setPalette(true);
  };

  // The composer: mounted only while open, like the palette. Its request's
  // progress lives in the flow (`composer.ts`), not in the view, so Cancel
  // closing the view never stops a create already running on the host.
  const composer = createComposerFlow({
    toast: (message) => toast(message),
    restoreFocus: focusShown,
    sessions: () => sessions(),
    select: (sessionId) => dispatch(selectRowIntent({ session: sessionId })),
  });
  // The "+" menu's agents: one more agent in a pane's shown worktree
  // (`new_agent.ts`); the host opens and focuses its tab.
  const newAgent = createNewAgentFlow({
    toast: (message) => toast(message),
    sessions: () => sessions(),
    select: (sessionId) => dispatch(selectRowIntent({ session: sessionId })),
  });
  // The row menu's Delete: a confirmation first, then the host's delete.
  const deletion = createDeleteFlow({ toast: (message) => toast(message) });
  /** A modal owns the keyboard: the composer, or the delete confirmation. */
  const modalOpen = () => composer.open() || deletion.target() !== null;

  /** The Needs you card the last Cmd+U revealed, so the next press moves on. */
  let jumped: string | null = null;
  /**
   * Cmd+U: reveal the next agent in the board's Needs you column. On the board
   * its card takes focus and its row is selected, as a click on it does;
   * anywhere else its tab is shown, or opened when it has none. Every send
   * goes after `answer_home`, so from Settings or the Inbox nothing is
   * refused. With nothing waiting it says so: Orca has no such chord, so
   * there is no silence to match, and a chord that does nothing reads as dead.
   */
  const jumpToAttention = () => {
    const card = nextNeedsYou(columns(), jumped);
    if (card === null) {
      toast("Nobody needs you");
      return;
    }
    jumped = card.key;
    // No row: an ACP agent (`nextNeedsYou` skips any other), shown by its transcript.
    if (card.sessionId === null) {
      void invoke("answer_home").then(() => openTranscript(card.key));
      return;
    }
    if (showing("board")) {
      void answer(showIntents(card.sessionId, card.hasOpenRequest));
      [...document.querySelectorAll<HTMLElement>(".board-card")].find((node) => node.dataset.card === card.key)?.focus();
      return;
    }
    void showSession(card.sessionId);
  };
  /** Show `sessionId`'s tab, or open one when it has none: after
   * `answer_home`, so from Settings or the Inbox nothing is refused. The
   * open's refusal, if the host refused it; `null` when a tab was shown. */
  const showSession = async (sessionId: string): Promise<Refusal | null> => {
    const tab = tabs().find((one) => one.target.kind === "session" && one.target.id === sessionId);
    if (tab === undefined) return answer([openRowIntent({ session: sessionId })]);
    choose(tab);
    return null;
  };
  /** Hide the sidebar, or show it again. A hidden sidebar cannot keep the
   * keyboard: it goes to the shown terminal, or nowhere. */
  const toggleSidebar = () => {
    const hiding = sidebarShown();
    if (hiding && sidebar?.contains(document.activeElement)) {
      const key = active();
      if (key !== null && showing("terminal")) focusers.get(key)?.();
      else (document.activeElement as HTMLElement | null)?.blur();
    }
    setSidebarShown(!hiding);
  };

  const onAccelerator = (shell: Accelerator) => {
    // A modal owns the keyboard: no chord may switch or close a tab, or focus
    // a terminal, behind the open composer.
    if (modalBlocks(shell, modalOpen())) return;
    // Under the delete confirmation not even the chords the composer lets
    // through run: Mod+N would open a second modal over it.
    if (deletion.target() !== null) return;
    switch (shell.kind) {
      case "tab": {
        const tab = inView()[shell.index];
        if (tab) choose(tab);
        return;
      }
      case "prev":
      case "next": {
        const key = stepTab(inView(), active(), shell.kind === "next" ? 1 : -1);
        const tab = tabs().find((candidate) => candidate.key === key);
        if (tab) choose(tab);
        return;
      }
      case "close": {
        const tab = tabs().find((candidate) => candidate.key === active());
        if (tab) closeTab(tab);
        return;
      }
      case "split": {
        // Orca splits the pane in view; behind a page there is none.
        if (!showing("terminal")) return;
        const current = layout();
        const next = splitGroup(current, current.focused, shell.direction);
        // A pane of one tab has nothing to split out: open the session to
        // put beside it first, then split it.
        if (next === current) toast("Open another tab in this pane to split it");
        else applyLayout(next);
        return;
      }
      case "terminal":
        void shellTabs.open();
        return;
      // Orca's switcher runs over the terminal view only
      // (`orca:src/renderer/src/components/tab-bar/RecentTabSwitcher.tsx:59-61`).
      case "recent":
        if (showing("terminal") && !palette()) switcher.step(shell.step);
        return;
      case "reopen":
        reopenClosed();
        return;
      case "attention":
        jumpToAttention();
        return;
      // From the worktree on screen, as Orca steps from the active one; the
      // host's selection follows a shown terminal a frame later.
      case "worktree": {
        const from = steppedTo ?? shownSession() ?? sessions()?.selected_session_id ?? null;
        const next = stepWorktree(sessions(), from, shell.step, readCollapsed(safeStorage()));
        if (next === null) return;
        // Tabless: its tab opens when the host answers, and `activate` then
        // forgets this. A refused open opens nothing, so it is forgotten then.
        if (!tabs().some((tab) => tab.target.kind === "session" && tab.target.id === next)) steppedTo = next;
        void showSession(next).then((refusal) => {
          if (refusal !== null && steppedTo === next) steppedTo = null;
        });
        return;
      }
      case "sidebar":
        toggleSidebar();
        return;
      // ponytail: the host switcher is R1.
      case "hosts":
        return;
      // Answered by the terminal that has focus; outside one there is no
      // selection to copy, nowhere to paste, and no pane to clear.
      case "copy":
      case "paste":
      case "clear":
        return;
      case "palette":
        togglePalette();
        return;
      case "new":
        composer.openComposer();
        return;
    }
  };

  const [toasts, setToasts] = createSignal<{ id: number; text: string }[]>([]);
  let toastId = 0;
  const toast = (text: string) => {
    const id = ++toastId;
    setToasts((shown) => [...shown, { id, text: toastLine(text) }]);
    setTimeout(() => setToasts((shown) => shown.filter((entry) => entry.id !== id)), TOAST_MS);
  };

  // "New terminal" and every tab close: a shell's tab ends its shell. Mod+T
  // opens in the focused pane's worktree, by the rule each pane's "+" applies
  // to its own shown tab.
  const plusTarget = () =>
    worktreeTarget(shownTargetOf(showing("terminal"), tabs(), active()), sessions()?.selected_session_id ?? null);
  const shellTabs = createShellTabs({ target: plusTarget, toast });

  // The accelerators work outside a terminal too; a terminal marks the ones
  // it handled, so they do not run twice.
  const onKey = shellKeydown({ mac: MAC, modalOpen, run: onAccelerator });
  window.addEventListener("keydown", onKey);
  onCleanup(() => window.removeEventListener("keydown", onKey));

  const openSession = (id: string) => openRow({ session: id });
  /** The select-only intent for the shown terminal's row, or none when no
   * terminal is shown: what a sidebar row menu sends after it moves the
   * selection, so the answer banner stays that terminal's. */
  const shownRowIntents = (): RendererIntent[] => {
    const key = active();
    const back = showing("terminal") && key !== null ? selectIntentFor(tabs(), key) : null;
    return back === null ? [] : [back];
  };

  onMount(async () => {
    setSidecar(await invoke<SidecarState>("sidecar_state"));
    showTabs(await invoke<TabsView>("terminal_tabs"));

    // A timer, not an animation frame: a hidden window still drains, so the
    // queue never grows while nobody looks.
    let queue: FrameBatch_Serialize[] = [];
    const drain = () => {
      const host = peer();
      // The first batches can arrive before `subscribe` answers: hold them.
      if (host === undefined) {
        setTimeout(drain, DRAIN_MS);
        return;
      }
      const batches = queue;
      queue = [];
      store.applyDrain(host, batches);
      // What the renderer applied, for the proof harness to read from the
      // log. Names and a count, never a body, and never a drain that carried
      // nothing for this window.
      const applied = [...new Set(batches.flatMap(({ frames }) => frames.map((frame) => frame.section)))];
      // A drain carrying nothing but the loading flag is not something the
      // renderer applied for a reader to see, and the window asks for a scan
      // on a cadence, so reporting it would append a line forever.
      if (applied.some((section) => section !== "workspace_load")) {
        void invoke("renderer_applied", {
          sections: applied,
          sessions: allSessions(store.section(host, "sessions")).length,
          board: agentStateCounts(store.section(host, "agent_status")?.view?.cards ?? []),
          inbox: inboxCounts(store.section(host, "inbox")),
        });
      }
    };
    const frames = new Channel<FrameBatch_Serialize>();
    frames.onmessage = (batch) => {
      if (queue.push(batch) === 1) setTimeout(drain, DRAIN_MS);
    };
    const answered = await invoke<HostId>("subscribe", { frames, sections: SUBSCRIBED });
    // A `host` event that landed while `subscribe` was in flight is newer than
    // this answer: keep it.
    if (peer() === undefined) setPeer(answered);
  });

  // In this node the window holds exactly one host: this machine's daemon.
  const host = peer;
  const sessions = () => shellSessions(store, host());
  const fleet = () => shellFleet(store, host());
  const agentStatus = () => shellAgentStatus(store, host());
  const gitView = () => shellGitView(store, host());
  const usage = () => shellUsage(store, host());
  const usageStale = createMemo(() => ROOT_SELECTORS.usageStale(store, host()));
  // The board's columns, projected ONCE and read by the board and by the
  // status bar's two counts, so the footer moves on the frame the board does
  // and a session is in exactly one of "need you" and "idle".
  const columns = createMemo(() => boardColumns(agentStatus(), fleet(), sessions(), acks()), undefined, {
    equals: sameColumns,
  });
  const sessionsStale = createMemo(() => ROOT_SELECTORS.sessionsStale(store, host()));
  const gitViewStale = createMemo(() => ROOT_SELECTORS.gitViewStale(store, host()));
  const loading = createMemo(() => ROOT_SELECTORS.workspacesLoading(store, host()));
  const elsewhere = createMemo(() => ROOT_SELECTORS.attentionElsewhere(store, host()));
  const shell = () => (host() ? store.section(host()!, "shell") : undefined);
  const config = () => shellConfig(store, host());
  const hangar = () => shellHangar(store, host());
  /** The reducer is on its Config screen, which is the settings page. */
  const settings = createMemo(() => shell()?.current_screen === SURFACES.settings);
  /** The reducer is on its Inbox screen, which is the inbox page (D3p-c). */
  const inboxOpen = createMemo(() => shell()?.current_screen === SURFACES.inbox);
  const inbox = () => shellInbox(store, host());
  const inboxUnread = createMemo(() => ROOT_SELECTORS.inboxUnread(store, host()));
  const ask = () => fleet()?.ask_state;
  const question = createMemo(() => questionFor(sessions()));
  /** The session whose terminal the work area shows: `undefined` when no
   * terminal is shown, `null` for a tab of no session (`questionOver`). */
  const shownSession = createMemo(() => shownSessionOf(showing("terminal"), tabs(), active()));

  // The reducer speaks through its notices: a refused send says why in the
  // reducer's own words (a daemon that is gone, a native picker, nothing typed),
  // so the window shows each new one as a toast rather than a dead button. New
  // by identity, not by count: the list is capped and drained from the front.
  let heard: string[] = [];
  createEffect(() => {
    const notices = shell()?.notifications ?? [];
    // A success notice is the reducer congratulating itself ("Workspaces
    // loaded"), and the window rescans on a cadence: only what went wrong, or
    // what the person needs to know, becomes a toast.
    for (const notice of newNotices(heard, notices)) {
      if (notice.notification_type !== "Success") toast(label(notice.message));
    }
    heard = notices.map(noticeKey);
  });
  // Another surface answered first: the row reads delivered, and the winner is
  // named once, in a toast, however many frames repeat it.
  createEffect(
    on(
      () => {
        const phase = phaseOf(ask());
        return phase.kind === "already_answered" ? [ask()?.request, phase.by].join("\n") : null;
      },
      (winner, previous) => {
        if (winner !== null && winner !== previous) toast(`Already answered by ${winner.slice(winner.indexOf("\n") + 1)}`);
      },
    ),
  );

  /** The glyph a session tab shows before its title: `null` for a bare tmux
   * tab, or a session this window has not listed yet, the same answer the
   * sidebar row and the board card read for the very same agent. */
  const tabStatus = (tab: Tab) =>
    statusForTarget(tab.target, allSessions(sessions()), agentStatus()?.view?.cards ?? [], fleet()?.fleet_metadata, acks());

  /** A tab's title: its session's name when the sidebar knows it. */
  const title = (tab: Tab) => {
    const target = tab.target;
    if (target.kind === "shell") return label(shellTitle(target));
    const session = target.kind === "session" ? allSessions(sessions()).find((row) => row.id === target.id) : undefined;
    return label(session?.name ?? target.tmux);
  };

  const banner = () => sidecarBanner(sidecar());

  return (
    <main class="shell">
      {/* Everything but the modals and the toasts: inert while the
          composer or the delete confirmation is open, so neither a click nor
          Tab can reach the shell behind the modal. `display: contents` keeps
          the layout. */}
      <div
        class="shell-content"
        ref={(element) => createEffect(() => element.toggleAttribute("inert", modalOpen()))}
      >
        <Titlebar
          mac={MAC}
          searchOpen={palette()}
          onSearch={togglePalette}
          inboxOpen={inboxOpen()}
          inboxUnread={inboxUnread()}
          onInbox={() => (inboxOpen() ? closeInbox() : openInbox())}
          settingsOpen={settings()}
          onSettings={() => (settings() ? closeSettings() : openSettings())}
        />
        <Show when={sidecar().state !== "connected"}>
          <div class={`banner ${sidecar().state}`} role="status">
            <span>{banner()}</span>
            <Show when={retryable(sidecar())}>
              <span class="actions">
                <Show when={(sidecar() as { has_log?: boolean }).has_log}>
                  <button
                    type="button"
                    onClick={async () => setLog((await invoke<string | null>("show_log")) ?? "")}
                  >
                    Show log
                  </button>
                </Show>
                <button
                  type="button"
                  onClick={() => {
                    setLog(null);
                    void invoke("retry_sidecar");
                  }}
                >
                  Retry
                </button>
              </span>
            </Show>
          </div>
        </Show>
        <Show when={log() !== null}>
          <pre class="sidecar-log" aria-label="Sidecar log">
            {log()}
          </pre>
        </Show>
        <div class="body">
          <Sidebar
            sessions={sessions()}
            stale={sessionsStale()}
            loading={loading()}
            pending={composer.pending()}
            cards={agentStatus()?.view?.cards ?? []}
            fleetMetadata={fleet()?.fleet_metadata}
            acks={acks()}
            onOpen={openSession}
            onNew={composer.openComposer}
            labels={shellLabels(store, host())?.session_label_store}
            // The host checks and keeps the name; no reducer row runs, so
            // nothing walks home first, as the Delete confirmation does not.
            onRename={(id, name) =>
              invoke("session_rename", { id, name }).then(
                () => null,
                (why: unknown) => String(why),
              )
            }
            onRowPick={(pick) =>
              // Through `answer`, home first: the sidebar is drawn over
              // Settings and the Inbox, whose screens refuse a row.
              runRowPick(pick, {
                open: (id) => void answer([openRowIntent({ session: id })]),
                run: (intents) => void answer(intents),
                copy: (text) => void invoke("clipboard_write", { text }),
                reselect: shownRowIntents,
                confirmDelete: deletion.open,
              })
            }
            ref={(element) => {
              sidebar = element;
              createEffect(() => element.toggleAttribute("hidden", !sidebarShown()));
            }}
          />
          <section class="workarea">
            <nav class="tabs" aria-label="Board and terminals" data-host-answers={hostAnswers()} data-host-focus={hostFocus()}>
              <span class="tab board-tab" classList={{ active: showing("board") }}>
                <button
                  type="button"
                  class="tab-title"
                  aria-current={showing("board") ? "page" : undefined}
                  onClick={() => {
                    closeTranscript();
                    setPane("board");
                  }}
                >
                  Board
                </button>
              </span>
              <span class="tab review-tab" classList={{ active: showing("review") }}>
                <button
                  type="button"
                  class="tab-title"
                  aria-current={showing("review") ? "page" : undefined}
                  onClick={() => {
                    closeTranscript();
                    setPane("review");
                  }}
                >
                  Review
                </button>
              </span>
              <span class="tab commits-tab" classList={{ active: showing("commits") }}>
                <button
                  type="button"
                  class="tab-title"
                  aria-current={showing("commits") ? "page" : undefined}
                  onClick={() => {
                    closeTranscript();
                    setPane("commits");
                  }}
                >
                  Commits
                </button>
              </span>
              <span class="tab stats-tab" classList={{ active: showing("stats") }}>
                <button
                  type="button"
                  class="tab-title"
                  aria-current={showing("stats") ? "page" : undefined}
                  onClick={() => {
                    closeTranscript();
                    setPane("stats");
                  }}
                >
                  Stats
                </button>
              </span>
              {/* The terminal panes, whose tabs are in each pane's own strip. */}
              <span class="tab terminals-tab" classList={{ active: showing("terminal") }}>
                <button
                  type="button"
                  class="tab-title"
                  aria-current={showing("terminal") ? "page" : undefined}
                  onClick={showTerminals}
                >
                  Terminals
                </button>
              </span>
              {/* The ACP card's own place in the strip, where the session's
                  terminal tab would be if it had a pane. */}
              <Show when={transcriptKey()}>
                {(key) => (
                  <span class="tab transcript-tab active" data-state="transcript">
                    <button type="button" class="tab-title" aria-current="page">
                      {key()}
                    </button>
                    <button
                      type="button"
                      class="tab-close"
                      aria-label={`Close ${key()}`}
                      onClick={() => {
                        closeTranscript();
                        setPane("board");
                      }}
                    >
                      ×
                    </button>
                  </span>
                )}
              </Show>
            </nav>
            {/* One banner per open request, latched for a short grace across
                frames that carry none (#1266): `AnswerSlot`. */}
            {/* Keyed by the shown terminal's session, so switching terminals
                drops a latched banner at once instead of holding the last
                session's question over the next one's pane for the grace. */}
            <For each={[shownSession()]}>
              {() => <AnswerSlot question={questionOver(question(), shownSession())} ask={ask()} run={async (intents) => void (await answer(intents))} />}
            </For>
            <Show when={transcriptKey()}>
              {(key) => (
                <AcpCard
                  sessionKey={key()}
                  view={transcriptView(fleet(), key())}
                  onClose={() => {
                    closeTranscript();
                    setPane("board");
                  }}
                />
              )}
            </Show>
            <Show when={inboxOpen()}>
              <Inbox
                inbox={inbox()}
                onChoose={dispatch}
                onClose={() => {
                  closeInbox();
                  setPane("board");
                }}
              />
            </Show>
            <Show when={settings()}>
              <SettingsPage
                config={config()}
                revision={configRevision(store, host())}
                hangar={hangar()}
                sidecar={sidecar()}
                setup={setup()}
                run={(intents) => void run(intents)}
                onSetupWrite={setupWrite}
                onRefreshSetup={refreshSetup}
                theme={theme.preference()}
                onTheme={theme.set}
                onClose={() => {
                  closeSettings();
                  setPane("board");
                }}
              />
            </Show>
            <Show when={showing("review")}>
              <Review gitView={gitView()} stale={gitViewStale()} onChoose={dispatch} />
            </Show>
            <Show when={showing("commits")}>
              <Commits gitView={gitView()} stale={gitViewStale()} onChoose={dispatch} />
            </Show>
            <Show when={showing("stats")}>
              <Stats usage={usage()} stale={usageStale()} />
            </Show>
            <Show when={showing("board")}>
              <Board
                agentStatus={agentStatus()}
                fleet={fleet()}
                sessions={sessions()}
                elsewhere={elsewhere()}
                columns={columns()}
                onAck={ackSession}
                onChoose={dispatch}
                onOpenTranscript={openTranscript}
              />
            </Show>
            <Show when={showing("terminal") && tabs().length === 0}>
              <EmptyPane view={emptyPaneView(sessions())} onOpen={openSession} />
            </Show>
            {/* The split panes. Each terminal is keyed by tab key, not by the
                tab object each event replaces: it stays mounted, and keeps its
                buffer, while its tab is listed, whichever group it moves to. */}
            <Show when={tabs().length > 0}>
              <Panes
                layout={layout()}
                tabs={tabs()}
                shown={showing("terminal")}
                title={title}
                status={tabStatus}
                onChoose={choose}
                onClose={closeTab}
                onLayout={applyLayout}
                onFocusGroup={focusPaneGroup}
                onCloseGroup={closePaneGroup}
                stripEnd={(shown) => (
                  <TabCreateMenu
                    target={worktreeTarget(shown()?.target, sessions()?.selected_session_id ?? null)}
                    mac={MAC}
                    onNewTerminal={(target) => void shellTabs.open(target)}
                    agents={newAgent}
                    // This pane's, not the focused one's: a pick in another
                    // pane focuses that pane and its shown terminal, where
                    // the new tab then lands.
                    restoreFocus={() => {
                      const tab = shown();
                      if (tab !== undefined) activate(tab.key, false);
                      else focusShown();
                    }}
                  />
                )}
                terminal={(key, visible) => (
                  <Show when={tabs().find((tab) => tab.key === key)}>
                    {(tab) => (
                      <TerminalView
                        tab={tab()}
                        title={title(tab())}
                        active={visible()}
                        mac={MAC}
                        onAccelerator={onAccelerator}
                        onLeave={() => {
                          // Esc Esc asks for the sidebar: a hidden one shows.
                          setSidebarShown(true);
                          sidebar?.focus();
                        }}
                        focusRef={(focus) => focusers.set(key, focus)}
                        theme={theme.painted()}
                      />
                    )}
                  </Show>
                )}
              />
            </Show>
          </section>
        </div>
        <Statusbar
          host={host()}
          sidecar={sidecar()}
          needsYou={countIn(columns(), "needs")}
          idle={countIn(columns(), "idle")}
          // A development build shows frames the store refused (#1132).
          framesIgnored={import.meta.env.DEV ? store.framesIgnored() : undefined}
        >
          <UsageSegment
            usage={usage()}
            stale={usageStale()}
            onOpen={() => {
              closeTranscript();
              setPane("stats");
            }}
          />
        </Statusbar>
        <Show when={palette()}>
          <Palette sessions={sessions()} onChoose={dispatch} onClose={closePalette} />
        </Show>
        {switcher.view()}
      </div>
      <Show when={composer.open()}>
        <Composer
          sessions={sessions()}
          state={composer.state()}
          onSubmit={composer.submit}
          onClose={composer.closeComposer}
        />
      </Show>
      <Show when={deletion.target()}>
        {(target) => (
          <DeleteDialog
            target={target()}
            state={deletion.state()}
            onConfirm={deletion.confirm}
            onClose={deletion.close}
          />
        )}
      </Show>
      <div class="toasts" aria-live="polite">
        <Show when={updateLine(updatePhase())}>{(line) => <div class="toast update-status">{line()}</div>}</Show>
        <For each={toasts()}>{(entry) => <div class="toast">{entry.text}</div>}</For>
      </div>
    </main>
  );
}

// The theme control the settings page reads and sets (Appearance > Theme).
// The host keeps a copy so the next launch's window opens in the pick; a copy
// that fails to land costs that launch's first frame, nothing else.
const theme = startTheme((preference) => void invoke("theme_set", { preference }).catch(() => {}));
render(() => <Shell />, document.getElementById("root")!);
