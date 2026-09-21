# Documentation

This repository is the core of the product: the Rust workspace, the scripts
that build and prove it, the Claude Code hooks compiled into the binary, and
the macOS Fleet app. The documentation hub, the website and the research
notes are not here yet.

Everything below is still in this repository's **history** (nothing was
rewritten; `git log -- <path>` and `git show <sha>:<path>` still work), and
still live at the paths named in `stevengonsalvez/agents-in-a-box` on branch
`v2` until it is ported.

| left behind | what it held |
|---|---|
| `docs/` | The documentation hub: product, TUI, toolkit, plugin, knowledge, contributing and reference guides, the hangar phase and renovation notes, contracts, diagrams and journey assets |
| `research/` | Exploration notes and spikes |
| `explainers/` | Generated HTML walkthroughs |
| `website/` | The Astro + Starlight site |
| `reflect-kb/` | The reflect knowledge base |
| `plans/` | Design plans and acceptance criteria, including `plans/skill-manager/spec.md` |
| `.agents/`, `.claude/`, `.claude-plugin/` | Agent-harness configuration and the plugin marketplace manifest |
| `plugins/` other than `ainb-hooks/` | The `ainb-fleet`, `caveman-stats` and `illustration` Claude Code plugins |

What did NOT leave: the three generated references the build gates diff
against live at `man/ainb.1`, `man/cli.md` and `man/keyboard-shortcuts.md`.

Each folder is ported back as its own pull request when it is wanted.
