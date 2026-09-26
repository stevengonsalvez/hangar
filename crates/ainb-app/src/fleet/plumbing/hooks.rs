// ABOUTME: Idempotent read-preserve-modify-write merge of the ATC lifecycle hook
// set into Claude Code's `~/.claude/settings.json`.
//
// agent-deck injects its orchestration hooks by *merging* a managed block into
// the user's existing settings, never by overwriting — so a host that already
// runs the reflect plugin's `Stop`/`PreCompact`/`SessionStart`/`UserPromptSubmit`
// hooks and the ainb-hooks/notifyd `Notification`/`Stop` hooks keeps all of them.
// This module mirrors that contract for ainb:
//
//   * Every ATC-managed hook entry is tagged `"_ainb_atc_managed": true` so a
//     re-install replaces exactly our prior entries and nothing else
//     (idempotent), and uninstall strips exactly ours.
//   * User-authored hooks AND other tools' hooks (reflect, notifyd) on the same
//     event are preserved verbatim — we append our entry to the event's array,
//     we never replace the array.
//   * The full lifecycle event set is added: `SessionStart` → waiting,
//     `UserPromptSubmit` → running (+ budget reset), `Stop` → waiting + drain,
//     `Notification`, `SessionEnd` → dead.
//
// The Stop hook is the load-bearing one: it runs the synchronous drain that
// turns child completions into the parent's next turn (see `drain.rs`). All
// other events just write the session's status file.

use serde_json::{Map, Value, json};

/// JSON key tagging a hook entry as ATC-managed (for idempotent re-merge +
/// clean uninstall). Stable — never rename without a migration.
pub const ATC_MANAGED_KEY: &str = "_ainb_atc_managed";

/// The lifecycle events ATC injects, in stable order, each paired with its
/// matcher. All documented Claude events use the all-matcher `""`, including
/// `PreToolUse`; the hook derives `tool_name` from its raw payload. This keeps
/// the durable source ledger complete while only `AskUserQuestion` enters the
/// blocking structured-answer broker. Each maps to the canonical hook script
/// with the matching `AINB_HOOK_EVENT`.
pub const ATC_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", ""),
    ("Setup", ""),
    ("InstructionsLoaded", ""),
    ("UserPromptSubmit", ""),
    ("UserPromptExpansion", ""),
    ("MessageDisplay", ""),
    ("PreToolUse", ""),
    ("PermissionRequest", ""),
    ("PostToolUse", ""),
    ("PostToolUseFailure", ""),
    ("PostToolBatch", ""),
    ("PermissionDenied", ""),
    ("Notification", ""),
    ("SubagentStart", ""),
    ("SubagentStop", ""),
    ("TaskCreated", ""),
    ("TaskCompleted", ""),
    ("Stop", ""),
    ("StopFailure", ""),
    ("TeammateIdle", ""),
    ("ConfigChange", ""),
    ("CwdChanged", ""),
    ("FileChanged", ""),
    ("WorktreeCreate", ""),
    ("WorktreeRemove", ""),
    ("PreCompact", ""),
    ("PostCompact", ""),
    ("SessionEnd", ""),
    ("Elicitation", ""),
    ("ElicitationResult", ""),
];

/// Default per-hook timeout (seconds) Claude allows before it kills the hook.
/// Fast lifecycle events resolve in well under a second.
const DEFAULT_HOOK_TIMEOUT: u64 = 10;

/// `PermissionRequest` is the SYNCHRONOUS approve/deny gate: the hook blocks on
/// the approve broker until a human decides. Its Claude-side timeout must exceed
/// the broker's own AWAIT timeout (600s) so the broker's deny-fallback answers
/// the hook cleanly before Claude ever hard-kills it.
const PERMISSION_HOOK_TIMEOUT: u64 = 660;

/// `PreToolUse` carries the SYNCHRONOUS AskUserQuestion gate: for that one
/// matcher the hook holds the tool call open on the approve broker until a
/// human answers from Fleet, releases to the native picker, or the broker's own
/// 640s fallback fires. Same budget and same reasoning as
/// [`PERMISSION_HOOK_TIMEOUT`] — Claude's ceiling must exceed the broker await
/// so the fallback answers cleanly before Claude hard-kills the hook.
///
/// THIS IS A CEILING, NOT A DELAY. Every other `PreToolUse` (every Bash, Read,
/// Edit …) returns in well under a second and is completely unaffected; raising
/// the cap costs them nothing. It was lowered to 10s when the blocking branch
/// was removed, which left the cap below the await the branch needs.
const STRUCTURED_ASK_HOOK_TIMEOUT: u64 = 660;

/// The Claude `timeout` (seconds) to register for `event`.
#[must_use]
fn timeout_for(event: &str) -> u64 {
    match event {
        "PermissionRequest" => PERMISSION_HOOK_TIMEOUT,
        "PreToolUse" => STRUCTURED_ASK_HOOK_TIMEOUT,
        _ => DEFAULT_HOOK_TIMEOUT,
    }
}

/// Single-quote a string for POSIX shell command interpolation.
///
/// Hook script paths can live under user homes with spaces or shell metacharacters,
/// so the generated command must pass the path as one literal argv.
#[must_use]
fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Build the ATC-managed hook entry for `event` (scoped to `matcher`), pointing
/// at `hook_script`.
///
/// The command sets `AINB_HOOK_EVENT=<event>` so the shared script branches
/// correctly, `AINB_HOOK_MATCHER=<matcher>` so the appended event line records
/// its discriminator (e.g. `AskUserQuestion`), and `AINB_MANAGED=atc` so it
/// knows it is running under ATC management (events.jsonl append + inbox writes).
/// Returns a single matcher block carrying one command hook — the Claude
/// settings shape (matcher `""` = all events of this type).
#[must_use]
pub fn managed_entry(event: &str, matcher: &str, hook_script: &str) -> Value {
    let hook_script = shell_quote(hook_script);
    let command = format!(
        "AINB_HOOK_EVENT={event} AINB_HOOK_MATCHER={matcher} AINB_MANAGED=atc {hook_script}"
    );
    json!({
        ATC_MANAGED_KEY: true,
        "matcher": matcher,
        "hooks": [
            { "type": "command", "command": command, "timeout": timeout_for(event) }
        ]
    })
}

/// JSON key marking a managed entry as the HTTP transport's (hooks-and-answers).
///
/// A second key rather than a new value for [`ATC_MANAGED_KEY`]: a v1.29.0
/// binary reads that key as a bool, so it still recognises, replaces and
/// strips these entries. That is the rollback-by-downgrade guarantee.
pub const HOOK_TRANSPORT_KEY: &str = "_ainb_hook_transport";

/// Which script the managed hooks run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookTransport {
    /// `notify.sh` on the 30 events in [`ATC_EVENTS`], through notifyd and
    /// `ainb fleet atc hook`.
    Legacy,
    /// `ainb-hook.sh` on the 15 events in
    /// [`ainb_hangar_proto::hooks::CLAUDE_HOOK_EVENTS`], posting to the hangar
    /// daemon's loopback listener.
    Http,
}

impl HookTransport {
    /// The spelling used by `--hooks` and the transport marker file.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Http => "http",
        }
    }

    /// Parse `legacy` or `http`.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "legacy" => Some(Self::Legacy),
            "http" => Some(Self::Http),
            _ => None,
        }
    }
}

/// Build the HTTP-transport managed entry for `event`, pointing at
/// `hook_script` (`ainb-hook.sh`). Keeps `AINB_MANAGED=atc` in the command and
/// the bool [`ATC_MANAGED_KEY`], so every older strip rule still matches it.
#[must_use]
pub fn http_managed_entry(event: &str, hook_script: &str) -> Value {
    let hook_script = shell_quote(hook_script);
    let command =
        format!("AINB_AGENT=claude AINB_HOOK_EVENT={event} AINB_MANAGED=atc {hook_script}");
    json!({
        ATC_MANAGED_KEY: true,
        HOOK_TRANSPORT_KEY: HookTransport::Http.as_str(),
        "matcher": "",
        "hooks": [
            { "type": "command", "command": command, "timeout": timeout_for(event) }
        ]
    })
}

/// Which transport the managed entries in `settings` belong to: `Http` if any
/// entry carries the HTTP marker, `Legacy` if any other managed entry exists,
/// `None` when nothing is managed.
#[must_use]
pub fn installed_transport(settings: &Value) -> Option<HookTransport> {
    let hooks = settings.get("hooks").and_then(Value::as_object)?;
    let mut found = None;
    for arr in hooks.values().filter_map(Value::as_array) {
        for entry in arr.iter().filter(|e| is_atc_managed(e)) {
            if entry.get(HOOK_TRANSPORT_KEY).and_then(Value::as_str) == Some("http") {
                return Some(HookTransport::Http);
            }
            found = Some(HookTransport::Legacy);
        }
    }
    found
}

/// Replace every managed entry with the 15-event HTTP set. Every other hook
/// (user, reflect, notifyd, the ainb-hooks plugin) is kept verbatim.
#[must_use]
pub fn merge_http_into(settings: Value, hook_script: &str) -> Value {
    let mut settings = strip_managed(settings);
    let root = settings.as_object_mut().expect("strip_managed returns an object");
    let hooks = root.entry("hooks").or_insert_with(|| Value::Object(Map::new()));
    if !hooks.is_object() {
        *hooks = Value::Object(Map::new());
    }
    let hooks = hooks.as_object_mut().expect("ensured object");
    for event in ainb_hangar_proto::hooks::CLAUDE_HOOK_EVENTS {
        let arr = hooks.entry(event.to_string()).or_insert_with(|| Value::Array(Vec::new()));
        if !arr.is_array() {
            *arr = Value::Array(Vec::new());
        }
        arr.as_array_mut()
            .expect("ensured array")
            .push(http_managed_entry(event, hook_script));
    }
    settings
}

/// Install the legacy set from any starting point. From HTTP it first strips
/// the HTTP entries, so the result equals a fresh legacy install over the same
/// user hooks; otherwise it is exactly [`merge_into`].
#[must_use]
pub fn merge_legacy_into(settings: Value, hook_script: &str) -> Value {
    if installed_transport(&settings) == Some(HookTransport::Http) {
        merge_into(strip_managed(settings), hook_script)
    } else {
        merge_into(settings, hook_script)
    }
}

/// Remove every managed entry, and drop an event array only when removing
/// ours emptied it (a user's own empty array is left alone).
#[must_use]
pub fn strip_managed(mut settings: Value) -> Value {
    if !settings.is_object() {
        return Value::Object(Map::new());
    }
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return settings;
    };
    let mut emptied = Vec::new();
    for (event, arr) in hooks.iter_mut() {
        if let Some(a) = arr.as_array_mut() {
            let before = a.len();
            a.retain(|e| !is_atc_managed(e));
            if a.is_empty() && before > 0 {
                emptied.push(event.clone());
            }
        }
    }
    for event in emptied {
        hooks.shift_remove(&event);
    }
    settings
}

/// Merge the ATC lifecycle hooks into a parsed `settings.json` value, preserving
/// every existing hook (user-authored, reflect, notifyd). Idempotent: a second
/// call with the same `hook_script` produces an identical result.
///
/// Pure on `Value` so it is exhaustively unit-testable without touching disk.
#[must_use]
pub fn merge_into(mut settings: Value, hook_script: &str) -> Value {
    // Ensure a top-level object.
    if !settings.is_object() {
        settings = Value::Object(Map::new());
    }
    let root = settings.as_object_mut().expect("ensured object");

    // Ensure a `hooks` object.
    let hooks = root.entry("hooks").or_insert_with(|| Value::Object(Map::new()));
    if !hooks.is_object() {
        *hooks = Value::Object(Map::new());
    }
    let hooks = hooks.as_object_mut().expect("ensured object");

    for (event, matcher) in ATC_EVENTS {
        let entry = managed_entry(event, matcher, hook_script);
        let arr = hooks.entry((*event).to_string()).or_insert_with(|| Value::Array(Vec::new()));
        if !arr.is_array() {
            *arr = Value::Array(Vec::new());
        }
        let arr = arr.as_array_mut().expect("ensured array");
        // Drop any prior ATC-managed entry for this event (idempotent re-merge),
        // keep everything else (reflect / notifyd / user hooks).
        arr.retain(|e| !is_atc_managed(e));
        arr.push(entry);
    }

    settings
}

/// Strip every ATC-managed hook entry from a parsed `settings.json` value,
/// leaving all other hooks (reflect, notifyd, user) intact. Empty event arrays
/// are removed so uninstall leaves no noise.
#[must_use]
pub fn strip_from(mut settings: Value) -> Value {
    let Some(root) = settings.as_object_mut() else {
        return settings;
    };
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return settings;
    };
    for (_event, arr) in hooks.iter_mut() {
        if let Some(a) = arr.as_array_mut() {
            a.retain(|e| !is_atc_managed(e));
        }
    }
    // Drop now-empty event arrays.
    hooks.retain(|_, v| v.as_array().map(|a| !a.is_empty()).unwrap_or(true));
    settings
}

/// Whether a hook entry is one ATC manages (tagged, or — defensively — a command
/// that invokes the script with our `AINB_MANAGED=atc` marker).
#[must_use]
pub fn is_atc_managed(entry: &Value) -> bool {
    if entry.get(ATC_MANAGED_KEY).and_then(Value::as_bool).unwrap_or(false) {
        return true;
    }
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter().any(|h| {
                h.get("command")
                    .and_then(Value::as_str)
                    .map(|s| s.contains("AINB_MANAGED=atc"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A settings.json carrying BOTH the reflect plugin's hooks and the
    /// ainb-hooks/notifyd hooks — the exact coexistence the goal requires.
    fn settings_with_reflect_and_notifyd() -> Value {
        json!({
            "hooks": {
                "SessionStart": [
                    { "matcher": "", "hooks": [
                        { "type": "command", "command": "uv run ${CLAUDE_PLUGIN_ROOT}/skills/recall/hooks/session_start_recall.py" }
                    ]}
                ],
                "UserPromptSubmit": [
                    { "matcher": "", "hooks": [
                        { "type": "command", "command": "uv run ${CLAUDE_PLUGIN_ROOT}/skills/recall/hooks/user_prompt_submit_recall.py" }
                    ]}
                ],
                "PostToolUse": [
                    { "matcher": "", "hooks": [
                        { "type": "command", "command": "uv run ${CLAUDE_PLUGIN_ROOT}/hooks/posttooluse_minilearning.py" }
                    ]}
                ],
                "Stop": [
                    { "matcher": "", "hooks": [
                        { "type": "command", "command": "uv run ${CLAUDE_PLUGIN_ROOT}/hooks/stop_reflect.py" }
                    ]},
                    { "matcher": "", "hooks": [
                        { "type": "command", "command": "AINB_AGENT=claude /x/notify.sh" }
                    ]}
                ],
                "PreCompact": [
                    { "matcher": "", "hooks": [
                        { "type": "command", "command": "uv run ${CLAUDE_PLUGIN_ROOT}/hooks/precompact_reflect.py --auto --verbose" }
                    ]}
                ],
                "Notification": [
                    { "matcher": "", "hooks": [
                        { "type": "command", "command": "AINB_AGENT=claude /x/notify.sh" }
                    ]}
                ]
            }
        })
    }

    fn commands_for(settings: &Value, event: &str) -> Vec<String> {
        settings["hooks"][event]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .flat_map(|e| e["hooks"].as_array().cloned().unwrap_or_default())
                    .filter_map(|h| h["command"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn merge_preserves_reflect_and_notifyd_hooks() {
        let merged = merge_into(settings_with_reflect_and_notifyd(), "/x/notify.sh");

        // Reflect's SessionStart survives, ours is added.
        let ss = commands_for(&merged, "SessionStart");
        assert!(
            ss.iter().any(|c| c.contains("session_start_recall.py")),
            "reflect lost: {ss:?}"
        );
        assert!(
            ss.iter().any(|c| c.contains("AINB_HOOK_EVENT=SessionStart")),
            "ours missing: {ss:?}"
        );

        // Reflect's PostToolUse + PreCompact are untouched (we don't manage them).
        assert!(
            commands_for(&merged, "PostToolUse")
                .iter()
                .any(|c| c.contains("posttooluse_minilearning.py"))
        );
        assert!(
            commands_for(&merged, "PreCompact")
                .iter()
                .any(|c| c.contains("precompact_reflect.py"))
        );

        // Stop: reflect's stop_reflect AND notifyd's notify.sh AND ours all present.
        let stop = commands_for(&merged, "Stop");
        assert!(
            stop.iter().any(|c| c.contains("stop_reflect.py")),
            "reflect Stop lost: {stop:?}"
        );
        assert!(
            stop.iter().any(|c| c.contains("AINB_AGENT=claude /x/notify.sh")),
            "notifyd Stop lost: {stop:?}"
        );
        assert!(
            stop.iter().any(|c| c.contains("AINB_HOOK_EVENT=Stop")),
            "ATC Stop drain missing: {stop:?}"
        );

        // SessionEnd is added fresh (no prior hooks).
        assert!(
            commands_for(&merged, "SessionEnd")
                .iter()
                .any(|c| c.contains("AINB_HOOK_EVENT=SessionEnd"))
        );
    }

    #[test]
    fn merge_is_idempotent() {
        let once = merge_into(settings_with_reflect_and_notifyd(), "/x/notify.sh");
        let twice = merge_into(once.clone(), "/x/notify.sh");
        assert_eq!(once, twice, "second merge changed the result");
        // And exactly one ATC-managed Stop entry exists after re-merge.
        let atc_stop = twice["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| is_atc_managed(e))
            .count();
        assert_eq!(atc_stop, 1, "duplicate ATC Stop entries after re-merge");
    }

    #[test]
    fn merge_adds_full_lifecycle_event_set() {
        let merged = merge_into(json!({}), "/x/notify.sh");
        for (event, matcher) in ATC_EVENTS {
            let arr = merged["hooks"][event].as_array().expect("event added");
            let entry = arr.iter().find(|e| is_atc_managed(e)).expect("ATC entry present");
            assert_eq!(
                entry["matcher"].as_str(),
                Some(*matcher),
                "event {event} matcher mismatch"
            );
        }
        // PreToolUse is an all-matcher. The hook extracts tool_name from the
        // payload before deciding whether this is an AskUserQuestion.
        let pre = &merged["hooks"]["PreToolUse"];
        assert!(pre.is_array(), "PreToolUse installed");
        let cmd = pre[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(
            cmd.contains("AINB_HOOK_MATCHER="),
            "PreToolUse forwards its matcher: {cmd}"
        );
        // StopFailure installed (observational ERR source).
        assert!(merged["hooks"]["StopFailure"].is_array());
    }

    #[test]
    fn managed_hook_set_covers_every_documented_claude_event() {
        let names: Vec<_> = ATC_EVENTS.iter().map(|(event, _)| *event).collect();
        assert_eq!(
            names,
            vec![
                "SessionStart",
                "Setup",
                "InstructionsLoaded",
                "UserPromptSubmit",
                "UserPromptExpansion",
                "MessageDisplay",
                "PreToolUse",
                "PermissionRequest",
                "PostToolUse",
                "PostToolUseFailure",
                "PostToolBatch",
                "PermissionDenied",
                "Notification",
                "SubagentStart",
                "SubagentStop",
                "TaskCreated",
                "TaskCompleted",
                "Stop",
                "StopFailure",
                "TeammateIdle",
                "ConfigChange",
                "CwdChanged",
                "FileChanged",
                "WorktreeCreate",
                "WorktreeRemove",
                "PreCompact",
                "PostCompact",
                "SessionEnd",
                "Elicitation",
                "ElicitationResult",
            ]
        );
    }

    #[test]
    fn managed_entry_shell_quotes_hook_script_path() {
        let entry = managed_entry("Stop", "", "/Users/Stevie G/a'b/notify.sh");
        let cmd = entry["hooks"][0]["command"].as_str().unwrap();
        assert!(
            cmd.ends_with("'/Users/Stevie G/a'\\''b/notify.sh'"),
            "script path must be single shell argument: {cmd}"
        );
    }

    #[test]
    fn merge_on_empty_or_non_object_settings() {
        // Null / non-object settings are coerced to an object with hooks.
        let merged = merge_into(Value::Null, "/x/notify.sh");
        assert!(merged["hooks"]["Stop"].is_array());
        let merged2 = merge_into(json!("garbage-string"), "/x/notify.sh");
        assert!(merged2["hooks"]["Stop"].is_array());
    }

    #[test]
    fn strip_removes_only_atc_entries() {
        let merged = merge_into(settings_with_reflect_and_notifyd(), "/x/notify.sh");
        let stripped = strip_from(merged);

        // ATC entries gone everywhere.
        for (event, _matcher) in ATC_EVENTS {
            let atc = stripped["hooks"][event]
                .as_array()
                .map(|a| a.iter().filter(|e| is_atc_managed(e)).count())
                .unwrap_or(0);
            assert_eq!(atc, 0, "ATC entry survived strip on {event}");
        }
        // But reflect + notifyd survive.
        assert!(
            commands_for(&stripped, "SessionStart")
                .iter()
                .any(|c| c.contains("session_start_recall.py"))
        );
        assert!(commands_for(&stripped, "Stop").iter().any(|c| c.contains("stop_reflect.py")));
        assert!(commands_for(&stripped, "Stop").iter().any(|c| c.contains("notify.sh")));
        assert!(commands_for(&stripped, "Notification").iter().any(|c| c.contains("notify.sh")));
    }

    #[test]
    fn strip_then_merge_round_trips_to_merged() {
        // merge → strip → merge yields the same as a single merge (no drift).
        let base = settings_with_reflect_and_notifyd();
        let merged_once = merge_into(base.clone(), "/x/notify.sh");
        let round = merge_into(strip_from(merged_once.clone()), "/x/notify.sh");
        assert_eq!(merged_once, round);
    }

    fn managed_events(settings: &Value) -> Vec<String> {
        settings["hooks"]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(_, arr)| arr.as_array().unwrap().iter().any(is_atc_managed))
            .map(|(k, _)| k.clone())
            .collect()
    }

    #[test]
    fn http_set_is_the_fifteen_events_and_keeps_every_other_hook() {
        let legacy = merge_into(settings_with_reflect_and_notifyd(), "/x/notify.sh");
        let http = merge_http_into(legacy, "/x/ainb-hook.sh");
        let mut events = managed_events(&http);
        events.sort();
        let mut want: Vec<String> = ainb_hangar_proto::hooks::CLAUDE_HOOK_EVENTS
            .iter()
            .map(|e| (*e).to_string())
            .collect();
        want.sort();
        assert_eq!(events, want);
        assert!(events.iter().any(|e| e == "Notification"));
        assert!(events.iter().any(|e| e == "Elicitation"));
        // No legacy entry survives, and nothing of ours on a dropped event.
        for event in [
            "Setup",
            "PreCompact",
            "PermissionDenied",
            "ElicitationResult",
        ] {
            assert!(
                commands_for(&http, event).iter().all(|c| !c.contains("AINB_MANAGED")),
                "{event}"
            );
        }
        assert!(!http["hooks"].as_object().unwrap().contains_key("Setup"));
        // Reflect and notifyd hooks are untouched.
        assert!(
            commands_for(&http, "PreCompact")
                .iter()
                .any(|c| c.contains("precompact_reflect.py"))
        );
        let stop = commands_for(&http, "Stop");
        assert!(stop.iter().any(|c| c.contains("stop_reflect.py")));
        assert!(stop.iter().any(|c| c == "AINB_AGENT=claude /x/notify.sh"));
        assert!(stop.iter().any(|c| c.contains("ainb-hook.sh")));
        assert_eq!(installed_transport(&http), Some(HookTransport::Http));
    }

    #[test]
    fn http_entries_block_only_on_the_two_holding_events() {
        let http = merge_http_into(json!({}), "/x/ainb-hook.sh");
        for event in ainb_hangar_proto::hooks::CLAUDE_HOOK_EVENTS {
            let entry = &http["hooks"][event][0];
            let want = if matches!(event, "PermissionRequest" | "PreToolUse") {
                660
            } else {
                10
            };
            assert_eq!(entry["hooks"][0]["timeout"], want, "{event}");
            let cmd = entry["hooks"][0]["command"].as_str().unwrap();
            assert_eq!(
                cmd,
                format!(
                    "AINB_AGENT=claude AINB_HOOK_EVENT={event} AINB_MANAGED=atc '/x/ainb-hook.sh'"
                )
            );
        }
    }

    #[test]
    fn an_older_binarys_strip_rule_removes_http_entries() {
        // v1.29.0 strips by the bool tag or the AINB_MANAGED=atc marker.
        let http = merge_http_into(settings_with_reflect_and_notifyd(), "/x/ainb-hook.sh");
        let stripped = strip_from(http);
        assert_eq!(installed_transport(&stripped), None);
        assert_eq!(stripped, strip_from(settings_with_reflect_and_notifyd()));
    }

    #[test]
    fn legacy_after_http_equals_a_fresh_legacy_install() {
        let fresh = merge_into(settings_with_reflect_and_notifyd(), "/x/notify.sh");
        let there = merge_http_into(fresh.clone(), "/x/ainb-hook.sh");
        let back = merge_legacy_into(there, "/x/notify.sh");
        assert_eq!(
            serde_json::to_string_pretty(&back).unwrap(),
            serde_json::to_string_pretty(&fresh).unwrap()
        );
        assert_eq!(installed_transport(&back), Some(HookTransport::Legacy));
    }

    #[test]
    fn http_install_is_idempotent() {
        let once = merge_http_into(settings_with_reflect_and_notifyd(), "/x/ainb-hook.sh");
        let twice = merge_http_into(once.clone(), "/x/ainb-hook.sh");
        assert_eq!(once, twice);
    }

    #[test]
    fn strip_managed_keeps_a_users_own_empty_array() {
        // An empty array on an event we never manage is the user's; an array
        // we emptied by removing our own entry goes.
        let settings = json!({ "hooks": { "SomeFutureEvent": [] } });
        let merged = merge_into(settings, "/x/notify.sh");
        let stripped = strip_managed(merged);
        assert_eq!(stripped["hooks"]["SomeFutureEvent"], json!([]));
        assert!(!stripped["hooks"].as_object().unwrap().contains_key("Setup"));
    }

    #[test]
    fn transport_spellings_round_trip() {
        for t in [HookTransport::Legacy, HookTransport::Http] {
            assert_eq!(HookTransport::parse(t.as_str()), Some(t));
        }
        assert_eq!(HookTransport::parse("socket"), None);
    }
}
