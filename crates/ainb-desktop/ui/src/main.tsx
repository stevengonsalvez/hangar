import { render } from "solid-js/web";
import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { FrameBatch_Serialize, HostId } from "../../../ainb-app/bindings/AppState";
import { ackTurn, pruneAcks, readAcks, rowAckKey, writeAcks, type AckMap, type AckStorage } from "./acks.ts";
import { createFrameStore } from "./store.ts";
import { Stats } from "./stats.tsx";
import {
  configRevision,
  shellAgentStatus,
  shellConfig,
  shellFleet,
  shellGitView,
  shellHangar,
  shellInbox,
  shellSessions,
  shellUsage,
  SUBSCRIBED,
} from "./subscription.ts";
import { allSessions, label, NEED_YOU, ringFor } from "./sessions.ts";
import { ringSelector, ROOT_SELECTORS } from "./selectors.ts";
import { AcpCard } from "./acp.tsx";
import { transcriptIntent, transcriptView } from "./acp.ts";
import { AnswerSlot } from "./answer.tsx";
import { phaseOf, questionFor, type Refusal, sendInOrder } from "./answer.ts";
import { newNotices, noticeKey } from "./notices.ts";
import { terminal as updateDone, updateLine, type UpdatePhase } from "./update.ts";
import { Board } from "./board.tsx";
import { agentStateCounts } from "./board.ts";
import { CLOSE_INBOX, OPEN_INBOX, inboxCounts } from "./inbox.ts";
import { Inbox } from "./inbox.tsx";
import { SURFACES } from "./surfaces.ts";
import { Commits } from "./commits.tsx";
import { Review } from "./review.tsx";
import { Palette } from "./palette.tsx";
import { emptyPaneView } from "./pane_empty.ts";
import { EmptyPane } from "./pane_empty.tsx";
import { createComposerFlow } from "./composer.ts";
import { cardForSession, statusForTarget } from "./status.ts";
import { TerminalTab } from "./terminal_tab.tsx";
import { Composer } from "./composer.tsx";
import { Sidebar } from "./sidebar.tsx";
import { Titlebar } from "./titlebar.tsx";
import { Statusbar } from "./statusbar.tsx";
import { SettingsPage } from "./settings.tsx";
import { CLOSE_SETTINGS, OPEN_SETTINGS } from "./settings.ts";
import { banner as sidecarBanner, retryable, type SidecarState } from "./sidecar.ts";
import type { SetupView, SetupWrite } from "../../bindings/Desktop.ts";
import {
  accelerator,
  openRowIntent,
  rowOf,
  stepTab,
  acceleratorAllowedUnderModal,
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

/** The selectors summed into the status bar's one "N need you" count: one
 * per `NEED_YOU` kind, the same list `idleCount` leaves out, so a row is in
 * one total or the other and never both. */
const ATTENTION_SELECTORS = NEED_YOU.map(ringSelector);

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

  // Terminal tabs: the strip is the Rust side's; which tab shows is ours.
  const [tabs, setTabs] = createSignal<Tab[]>([]);
  const [active, setActive] = createSignal<string | null>(null);
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
  const tabKeys = createMemo(
    () => tabs().map((tab) => tab.key),
    [],
    { equals: (a, b) => a.length === b.length && a.every((key, i) => key === b[i]) },
  );
  let sidebar: HTMLElement | undefined;

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
      if (terminalMayTakeFocus({ palette: palette(), composer: composer.open(), byHost, active: document.activeElement })) {
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
  const activate = (key: string | null, byHost: boolean) => {
    setActive(key);
    if (key !== null) {
      setPane("terminal");
      closeTranscript();
      closeSettings();
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
    setTabs(view.tabs);
    for (const key of focusers.keys()) {
      if (!view.tabs.some((tab) => tab.key === key)) focusers.delete(key);
    }
    if (view.focus !== null) activate(view.focus, true);
    else if (!view.tabs.some((tab) => tab.key === active())) {
      // The shown tab ended (an unrelated tmux session died, say): point at
      // the next one WITHOUT leaving the board. `activate` means a person chose
      // a terminal; this is the strip tidying up after itself.
      const next = view.tabs[0]?.key ?? null;
      setActive(next);
      if (next !== null && pane() === "terminal") focusTab(next, true);
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
  const run = async (intents: RendererIntent[]) => {
    report(await sendInOrder(intents, (intent) => invoke<Refusal | null>("dispatch", { intent })));
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
  const answer = async (intents: RendererIntent[]) => {
    if (intents.length === 0) return;
    await invoke("answer_home");
    await run(intents);
  };
  /** The shell confirms in its own dialog, runs the write, and toasts the outcome. */
  const setupWrite = (write: SetupWrite) =>
    void invoke<boolean>("setup_write", { write }).then((ran) => {
      if (ran) refreshSetup();
    });

  // The palette is mounted only while it is open: each opening lists the
  // commands afresh, with the host's answer for which of them run now.
  const [palette, setPalette] = createSignal(false);
  const closePalette = () => {
    setPalette(false);
    const key = active();
    if (key !== null) focusers.get(key)?.();
    else sidebar?.focus();
  };
  const choose = (tab: Tab) => {
    if (tab.state === "detached") openRow(rowOf(tab.target));
    activate(tab.key, false);
  };
  /** Open the palette, or close it the way Esc does: the titlebar's search
   * button and the Cmd/Ctrl+Shift+K accelerator both send exactly this, so
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
    restoreFocus: () => {
      const key = active();
      if (key !== null) focusers.get(key)?.();
      else sidebar?.focus();
    },
  });

  const onAccelerator = (shell: Accelerator) => {
    // A modal owns the keyboard: no chord may switch or close a tab, or focus
    // a terminal, behind the open composer.
    if (composer.open() && !acceleratorAllowedUnderModal(shell)) return;
    switch (shell.kind) {
      case "tab": {
        const tab = tabs()[shell.index];
        if (tab) choose(tab);
        return;
      }
      case "prev":
      case "next":
        activate(stepTab(tabs(), active(), shell.kind === "next" ? 1 : -1), false);
        return;
      case "close": {
        const key = active();
        if (key !== null) void invoke("terminal_close", { key });
        return;
      }
      // ponytail: the attention jump is D2, the host switcher R1.
      case "attention":
      case "hosts":
        return;
      // Answered by the terminal that has focus; outside one there is no
      // selection to copy and nowhere to paste.
      case "copy":
      case "paste":
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
    setToasts((shown) => [...shown, { id, text: label(text) }]);
    setTimeout(() => setToasts((shown) => shown.filter((entry) => entry.id !== id)), TOAST_MS);
  };

  // The accelerators work outside a terminal too; a terminal marks the ones
  // it handled, so they do not run twice.
  const onKey = (event: KeyboardEvent) => {
    if (event.defaultPrevented) return;

    const shell = accelerator(event, MAC);
    // Under the composer, a chord the modal refuses is not the shell's to
    // swallow: left alone it reaches the focused field, so Ctrl+Shift+C and
    // Ctrl+Shift+V still copy and paste there off macOS.
    if (shell && !(composer.open() && !acceleratorAllowedUnderModal(shell))) {
      event.preventDefault();
      onAccelerator(shell);
    }
  };
  window.addEventListener("keydown", onKey);
  onCleanup(() => window.removeEventListener("keydown", onKey));

  const openSession = (id: string) => openRow({ session: id });

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
  /** ASK + APPROVE + WAIT + ERR: the status bar's one "N need you" amber
   * count. Each selector is its own memo first, so a drain that only moves
   * one of the four still wakes just that one before the sum recomputes. */
  const attentionCounts = ATTENTION_SELECTORS.map((select) => createMemo(() => select(store, host())));
  const needsYou = createMemo(() => attentionCounts.reduce((sum, count) => sum + count(), 0));
  const idle = createMemo(() => ROOT_SELECTORS.idleCount(store, host()));
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
    const session = target.kind === "session" ? allSessions(sessions()).find((row) => row.id === target.id) : undefined;
    return label(session?.name ?? target.tmux);
  };

  const banner = () => sidecarBanner(sidecar());

  return (
    <main class="shell">
      {/* Everything but the composer and the toasts: inert while the
          composer is open, so neither a click nor Tab can reach the shell
          behind the modal. `display: contents` keeps the layout. */}
      <div
        class="shell-content"
        ref={(element) => createEffect(() => element.toggleAttribute("inert", composer.open()))}
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
            ref={(element) => (sidebar = element)}
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
              <For each={tabs()}>
                {(tab) => (
                  <TerminalTab
                    tab={tab}
                    title={title(tab)}
                    active={showing("terminal") && tab.key === active()}
                    status={tabStatus(tab)}
                    onChoose={() => choose(tab)}
                    onClose={() => void invoke("terminal_close", { key: tab.key })}
                  />
                )}
              </For>
            </nav>
            {/* One banner per open request, latched for a short grace across
                frames that carry none (#1266): `AnswerSlot`. */}
            <AnswerSlot question={question()} ask={ask()} run={answer} />
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
                acks={acks()}
                onAck={ackSession}
                onChoose={dispatch}
                onOpenTranscript={openTranscript}
              />
            </Show>
            <Show when={showing("terminal") && tabs().length === 0}>
              <EmptyPane view={emptyPaneView(sessions())} onOpen={openSession} />
            </Show>
            {/* Keyed by tab key, not by the tab object each event replaces: a
                terminal stays mounted, and keeps its buffer, while its tab is listed. */}
            <For each={tabKeys()}>
              {(key) => (
                <Show when={tabs().find((tab) => tab.key === key)}>
                  {(tab) => (
                    <TerminalView
                      tab={tab()}
                      title={title(tab())}
                      active={showing("terminal") && key === active()}
                      mac={MAC}
                      onAccelerator={onAccelerator}
                      onLeave={() => sidebar?.focus()}
                      focusRef={(focus) => focusers.set(key, focus)}
                    />
                  )}
                </Show>
              )}
            </For>
          </section>
        </div>
        <Statusbar
          host={host()}
          sidecar={sidecar()}
          needsYou={needsYou()}
          idle={idle()}
          // A development build shows frames the store refused (#1132).
          framesIgnored={import.meta.env.DEV ? store.framesIgnored() : undefined}
        />
        <Show when={palette()}>
          <Palette sessions={sessions()} onChoose={dispatch} onClose={closePalette} />
        </Show>
      </div>
      <Show when={composer.open()}>
        <Composer
          sessions={sessions()}
          state={composer.state()}
          onSubmit={composer.submit}
          onClose={composer.closeComposer}
        />
      </Show>
      <div class="toasts" aria-live="polite">
        <Show when={updateLine(updatePhase())}>{(line) => <div class="toast update-status">{line()}</div>}</Show>
        <For each={toasts()}>{(entry) => <div class="toast">{entry.text}</div>}</For>
      </div>
    </main>
  );
}

startTheme();
render(() => <Shell />, document.getElementById("root")!);
