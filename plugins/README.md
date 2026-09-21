# Harness plugins

A **harness plugin** extends the *coding agent itself* (Claude Code, Codex
CLI, GitHub Copilot CLI) with skills, lifecycle hooks and notification
wiring. They are a different system from the TUI plugins, which extend `ainb`
and live in `crates/ainb-plugin-*` as workspace members:

```
┌──────────────────────────────┐        ┌──────────────────────────────┐
│  HARNESS plugins (this dir)  │        │  TUI plugins (crates/)       │
│  extend the coding AGENT     │   vs   │  extend the ainb TUI itself  │
│  Claude / Codex / Copilot    │        │  ratatui screens + commands  │
│  skills · hooks · notifs     │        │  JSON-RPC subprocess, ABI v2 │
│  installed via marketplace   │        │  spawned over stdio JSON-RPC │
└──────────────────────────────┘        └──────────────────────────────┘
```

See `CLAUDE.md` for the TUI plugin system.

## What is here

| Plugin | Kind | What it does |
|---|---|---|
| `ainb-hooks` | hooks | Captures documented Claude and Codex events into Hangar; routes only attention to the inbox |

`ainb-hooks` is the one harness plugin this repository carries, because it is
not only installed, it is **compiled in**: `crates/ainb-plugin-notifyd/src/install.rs`
bakes its `notify.sh`, `stall_guard.py` and the Claude, Codex, Copilot and
Antigravity manifests into the binary with `include_str!`, and two test
suites fire the real `notify.sh` against a real daemon.

One command writes all three hook formats:

```bash
ainb notifyd install --all          # = --claude --codex --copilot
```

`ainb notifyd …` and the standalone `ainb-notifyd …` binary are the same
entrypoint.

## What is not here yet

`ainb-fleet`, `caveman-stats` and `illustration` are in this repository's
history and still live in `stevengonsalvez/agents-in-a-box`; see
`docs/README.md`. `reflect` has always been sourced externally from
`github:stevengonsalvez/ainb-reflect-memory`, and `caveman` from its own
marketplace. All of them remain installable by name:

```bash
claude plugin marketplace add stevengonsalvez/agents-in-a-box
claude plugin install ainb-fleet@agents-in-a-box
```

or through `ainb init`, whose catalog is
`crates/ainb-app/src/setup/catalog.rs` (single source of truth for TUI
onboarding and the `ainb init` CLI).
