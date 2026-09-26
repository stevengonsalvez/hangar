// The answer journey, in one run against a real daemon and a real tmux
// session: a three-option question an agent's hook raises reaches the board
// and the attention banner, a person answers it from the window by picking
// option two, the daemon's one verified send types it into the agent's pane,
// and the daemon records the desktop as the surface that answered, which a
// second surface started against the same daemon then reads back.
//
//   hook line ──▶ events.jsonl ──▶ daemon ──▶ window: board card + banner
//                                    ▲                    │ option two
//                                    │ attention/answer   ▼
//   ainb web ── already answered ────┘◀──── session_list.ask.* (reducer)
//                                    │
//                                    ▼ tmux send-keys
//                              agent pane: "agent read: prod"
//
// The question is raised through the real hook, `ainb fleet atc hook`, from
// the session's own pane, under the session id ainb minted for the launch
// and handed to `claude --session-id`: the id the session record holds. The
// row takes the request by that id exactly, and the board's waiting card is
// keyed by it. Nothing is matched by worktree.

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { click } from "../support.js";
import { AINB_BIN, env, paneText, raiseHook, run, seeded } from "../world.js";

const QUESTION = "Ship to which environment?";
const OPTIONS = ["staging", "prod", "canary"];
/** Option two, as a person picks it: the second button, answered by label. */
const PICK = 1;

/** The AskUserQuestion call, as Claude hands it to its PreToolUse hook. */
const ASK = {
  event: "PreToolUse",
  matcher: "AskUserQuestion",
  payload: {
    tool_name: "AskUserQuestion",
    tool_input: { questions: [{ question: QUESTION, options: OPTIONS.map((label) => ({ label })) }] },
  },
};

/**
 * What the window asked the host to do and what became of it, in order: each
 * `renderer intent` line the desktop logged, as `{ command, outcome }`. A key
 * or a text reads `key` or `text(N chars)`, since their content is never
 * logged, and `outcome` is `dispatched` or `refused`.
 */
function intentsSent() {
  let lines = "";
  try {
    lines = run("sh", ["-c", 'cat "$1"/desktop.log* 2>/dev/null | grep "renderer intent"', "log", env().AINB_HANGAR_HOME]);
  } catch {
    return [];
  }
  return lines
    .split("\n")
    .filter(Boolean)
    .map((line) => ({
      command: line.match(/command="?([^"\s]+)/)?.[1] ?? "unknown",
      outcome: line.match(/outcome="?(\w+)/)?.[1] ?? "unknown",
    }));
}

/** The desktop's own log lines about answering, for a failure to show. */
function desktopLog() {
  try {
    return run("sh", ["-c", 'grep -hiE "answer|ask|attention" "$1"/desktop.log* 2>/dev/null | tail -15', "log", env().AINB_HANGAR_HOME]);
  } catch {
    return "(no desktop log)";
  }
}

/** Wait for `condition`, failing with what `explain` says at the deadline. */
async function settle(condition, timeout, explain) {
  try {
    await browser.waitUntil(condition, { timeout, interval: 100 });
  } catch {
    throw new Error(explain());
  }
}

/** The web dashboard, the second surface, while this spec runs. */
let web = null;

describe("answering from the window", () => {
  after(() => {
    // Its own child, stopped by its own pid.
    if (web !== null && web.exitCode === null) web.kill();
  });

  it("answers a daemon question from the banner and the daemon records the desktop", async () => {
    const target = seeded()[0];
    const stamp = Date.now();
    assert.ok(target.claude, "the world seeded a Claude session with a minted session id");
    const provider = target.claude;

    await $(".sidebar").waitForExist({ timeout: 90_000 });
    await browser.waitUntil(async () => !(await $(".banner").isExisting()), {
      timeout: 90_000,
      timeoutMsg: async () => `the sidecar never connected: ${await $(".banner").getText()}`,
    });
    await $(`.session-row[data-session="${target.id}"]`).waitForExist({ timeout: 60_000 });

    // Raised while the window is open, by a process that is not the window.
    raiseHook(target, ASK);

    // The board: the agent the id names sits in the waiting column.
    await click(".board-tab .tab-title");
    const card = `.board-column[data-state="waiting"] .board-card[data-card="claude:${provider}"]`;
    await $(card).waitForExist({
      timeout: 60_000,
      timeoutMsg: `no waiting card for claude:${provider} on the board`,
    });

    // The banner: the session's row carries the question, and selecting it
    // puts the question in front of the person.
    await click(`.session-row[data-session="${target.id}"]`);
    await $(".answer-banner[data-request]").waitForExist({
      timeout: 60_000,
      timeoutMsg: "the banner never showed the question",
    });
    // Queried again once it exists: an element looked up before it rendered
    // carries no id this driver can read an attribute through.
    const request = await $(".answer-banner[data-request]").getAttribute("data-request");
    assert.equal(await $(".answer-banner .answer-detail").getText(), QUESTION);
    const labels = await $$(".answer-banner .answer-option").map((option) => option.getText());
    assert.deepEqual(
      labels.map((text) => OPTIONS.find((label) => text.includes(label))),
      OPTIONS,
      "the banner offers the three options in order",
    );

    // Option two, clicked through the driver (#1192). The click sends the
    // reducer's own commands and nothing the window authored; the phase the
    // banner reads is the frame's `fleet.ask_state.phases` entry for this
    // request.
    const sentBefore = intentsSent().length;
    await click(`.answer-banner .answer-option[data-option="${PICK}"]`);
    let phase = "";
    let phaseLine = "";
    await settle(
      async () => {
        [phase, phaseLine] = await browser.execute((id) => {
          const line = document.querySelector(`.answer-banner[data-request="${id}"] .answer-phase`);
          return [line?.dataset.phase ?? "", line?.textContent ?? ""];
        }, request);
        return phase === "delivered";
      },
      60_000,
      () =>
        `the banner never read delivered (last phase: ${phase || "none"}: ${JSON.stringify(phaseLine)}; pane: ${paneText(target.tmux).trim().split("\n").slice(-3).join(" / ")})\n${desktopLog()}`,
    );

    // What the window sent for that pick, from the host's own log: the
    // reducer's session list commands ending in one pick by label, no cursor
    // move and no Enter (#1191), and nothing it authored.
    const sent = intentsSent().slice(sentBefore);
    const shown = sent.map(({ command, outcome }) => `${command}:${outcome}`).join(", ");
    assert.deepEqual(
      sent.filter(({ command }) => !command.startsWith("session_list.")),
      [],
      `the pick sent only session list commands: ${shown}`,
    );
    assert.deepEqual(
      sent.filter(({ outcome }) => outcome !== "dispatched"),
      [],
      `the host applied every one of them: ${shown}`,
    );
    const picks = sent.filter(({ command }) => command === "session_list.ask.pick");
    assert.deepEqual(picks, [{ command: "session_list.ask.pick", outcome: "dispatched" }], `pick: ${shown}`);
    assert.equal(sent.at(-1)?.command, "session_list.ask.pick", `the pick is the last thing sent: ${shown}`);
    assert.deepEqual(
      sent.filter(({ command }) => /^session_list\.ask\.(next|previous|enter)$/.test(command)),
      [],
      `no cursor move and no Enter rode with the pick: ${shown}`,
    );

    // The last mile: the agent in the pane read the label.
    await browser.waitUntil(() => paneText(target.tmux).includes(`agent read: ${OPTIONS[PICK]}`), {
      timeout: 30_000,
      timeoutMsg: `the answer never reached the agent in ${target.tmux}`,
    });

    // The daemon's record, read through its own RPC by a second surface on
    // the same daemon (#1193): the web dashboard's answer to the same question
    // loses to the desktop's, and the refusal names the winner. The answer
    // text itself is the pane's business, proven above.
    const port = 20_000 + (stamp % 20_000);
    const token = `e2e-${stamp}`;
    web = spawn(AINB_BIN, ["web", "--listen", `127.0.0.1:${port}`, "--token", token], {
      env: env(),
      stdio: "ignore",
    });
    let second = null;
    await browser.waitUntil(
      async () => {
        try {
          const response = await fetch(`http://127.0.0.1:${port}/api/answer`, {
            method: "POST",
            headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
            body: JSON.stringify({ attentionId: request, answer: OPTIONS[0] }),
          });
          second = await response.json();
          return true;
        } catch {
          return false;
        }
      },
      { timeout: 60_000, interval: 500, timeoutMsg: "the web dashboard never answered" },
    );
    assert.equal(second?.outcome, "already_answered", JSON.stringify(second));
    // `<surface>@<host>`: the surface is what the person sat at.
    assert.match(second.by, /^desktop@/, `the desktop answered, not the terminal: ${JSON.stringify(second)}`);

    // The board's card is the daemon's view of the agent, which stays waiting
    // until the agent's next hook event; the fixture agent fires none, so the
    // card is still there. The request itself reads answered, as the second
    // surface just found. Selecting the row opened its terminal tab, so the
    // board is brought back to be read.
    await click(".board-tab .tab-title");
    await $(card).waitForExist({
      timeout: 30_000,
      timeoutMsg: "the agent's card left the board",
    });
  });
});
