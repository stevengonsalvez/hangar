//! `ainb fleet atc setup --hooks=http|legacy` end to end, in a temp `$HOME`.
//!
//! Proves the operator-visible migration: http is refused while no daemon
//! publishes a hook endpoint, then rewrites the managed entries in place to the
//! 15-event set once one does, and `--hooks=legacy` puts the 30-event set back.

use std::path::{Path, PathBuf};
use std::process::Command;

use ainb_hangar_proto::hooks::{CLAUDE_HOOK_EVENTS, HookEndpoint, render_headers_file};

/// An instance name no real ATC uses, so teardown can only ever touch its own.
const INSTANCE: &str = "hooks-transport-test";

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

fn setup(home: &Path, transport: &str) -> serde_json::Value {
    let ainb_home = home.join("ainb");
    run(
        home,
        &[("AINB_HOME", &ainb_home), ("AINB_HANGAR_HOME", &ainb_home)],
        &[
            "setup",
            INSTANCE,
            "--no-spawn",
            "--no-heartbeat",
            "--hooks",
            transport,
        ],
    )
}

/// Run `ainb --format json fleet atc <args>` with `HOME=home` and `envs`.
fn run(home: &Path, envs: &[(&str, &Path)], args: &[&str]) -> serde_json::Value {
    let mut cmd = Command::new(ainb_bin());
    // A private tmux server under the temp home, never the user's: teardown
    // kills the instance's session by name.
    cmd.env("HOME", home)
        .env("TMUX_TMPDIR", home)
        .env_remove("TMUX")
        .env_remove("AINB_HOME")
        .env_remove("AINB_HANGAR_HOME")
        .env("AINB_BIN", ainb_bin())
        .args(["--format", "json", "fleet", "atc"])
        .args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("invoke ainb");
    assert!(
        out.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("atc prints JSON")
}

fn settings(home: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(home.join(".claude").join("settings.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn managed_commands(settings: &serde_json::Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (event, arr) in settings["hooks"].as_object().unwrap() {
        for entry in arr.as_array().unwrap() {
            if entry["_ainb_atc_managed"] == true {
                out.push((
                    event.clone(),
                    entry["hooks"][0]["command"].as_str().unwrap().to_string(),
                ));
            }
        }
    }
    out
}

fn publish_endpoint(home: &Path) {
    publish_endpoint_in(&home.join("ainb"));
}

fn publish_endpoint_in(hangar_home: &Path) {
    let dir = hangar_home.join("hangar");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    std::fs::write(dir.join("hook-headers"), render_headers_file("t")).unwrap();
    let endpoint = HookEndpoint {
        port: 45678,
        version: 1,
        pid: std::process::id(),
    };
    std::fs::write(dir.join("hook-endpoint.env"), endpoint.render_env_file()).unwrap();
}

fn user_hook(home: &Path) {
    let path = home.join(".claude").join("settings.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        r#"{"hooks":{"Stop":[{"matcher":"","hooks":[{"type":"command","command":"my-own-stop-hook"}]}]},"theme":"dark"}"#,
    )
    .unwrap();
}

#[test]
fn http_is_refused_without_a_daemon_endpoint_and_changes_nothing() {
    let home = tempfile::tempdir().unwrap();
    user_hook(home.path());
    let before = std::fs::read(home.path().join(".claude/settings.json")).unwrap();
    let report = setup(home.path(), "http");
    assert_eq!(report["lifecycle_hooks_installed"], false);
    assert_eq!(
        std::fs::read(home.path().join(".claude/settings.json")).unwrap(),
        before
    );
    assert!(!home.path().join("ainb/hooks/transport").exists());
}

#[test]
fn legacy_to_http_to_legacy_rewrites_the_managed_entries_in_place() {
    let home = tempfile::tempdir().unwrap();
    user_hook(home.path());

    setup(home.path(), "legacy");
    let legacy = managed_commands(&settings(home.path()));
    assert_eq!(legacy.len(), 30, "the v1.29.0 set");
    assert!(legacy.iter().all(|(_, c)| c.contains("notify.sh")));

    publish_endpoint(home.path());
    let report = setup(home.path(), "http");
    assert_eq!(report["lifecycle_hooks_installed"], true);
    let s = settings(home.path());
    let http = managed_commands(&s);
    let mut events: Vec<&str> = http.iter().map(|(e, _)| e.as_str()).collect();
    events.sort_unstable();
    let mut want: Vec<&str> = CLAUDE_HOOK_EVENTS.to_vec();
    want.sort_unstable();
    assert_eq!(
        events, want,
        "exactly the 15 events, Notification and Elicitation included"
    );
    assert!(
        http.iter()
            .all(|(_, c)| c.contains("ainb-hook.sh") && c.contains("AINB_MANAGED=atc"))
    );
    assert!(home.path().join("ainb/hooks/ainb-hook.sh").is_file());
    assert_eq!(
        std::fs::read_to_string(home.path().join("ainb/hooks/transport")).unwrap(),
        "http\n"
    );
    // The user's own hook and settings survive.
    assert_eq!(s["theme"], "dark");
    assert!(
        s["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["hooks"][0]["command"] == "my-own-stop-hook")
    );
    assert!(home.path().join(".claude/settings.json.ainb.bak").is_file());

    setup(home.path(), "legacy");
    let back = managed_commands(&settings(home.path()));
    assert_eq!(back, legacy, "legacy is restored exactly");
    assert!(
        !home.path().join("ainb/hooks/transport").exists(),
        "legacy is the absence of the marker"
    );
}

#[test]
fn teardown_of_the_last_instance_clears_the_http_marker() {
    let home = tempfile::tempdir().unwrap();
    user_hook(home.path());
    publish_endpoint(home.path());
    setup(home.path(), "http");
    let marker = home.path().join("ainb/hooks/transport");
    assert!(marker.exists());
    let ainb_home = home.path().join("ainb");
    let report = run(
        home.path(),
        &[("AINB_HOME", &ainb_home), ("AINB_HANGAR_HOME", &ainb_home)],
        &["teardown", INSTANCE],
    );
    assert_eq!(report["lifecycle_hooks_uninstalled"], true);
    assert!(
        !marker.exists(),
        "an orphan http marker keeps notify.sh down forever"
    );
    assert!(managed_commands(&settings(home.path())).is_empty());
}

#[test]
fn the_marker_goes_where_notify_sh_reads_it_when_the_homes_differ() {
    // AINB_HANGAR_HOME unset: the daemon and ainb-hook.sh use
    // ~/.agents-in-a-box, while notify.sh reads AINB_HOME first.
    let home = tempfile::tempdir().unwrap();
    let default_hangar = home.path().join(".agents-in-a-box");
    let ainb_home = home.path().join("elsewhere");
    publish_endpoint_in(&default_hangar);
    let report = run(
        home.path(),
        &[("AINB_HOME", &ainb_home)],
        &[
            "setup",
            INSTANCE,
            "--no-spawn",
            "--no-heartbeat",
            "--hooks",
            "http",
        ],
    );
    assert_eq!(report["lifecycle_hooks_installed"], true);
    assert_eq!(
        std::fs::read_to_string(ainb_home.join("hooks/transport")).unwrap(),
        "http\n",
        "notify.sh reads AINB_HOME"
    );
    assert_eq!(
        std::fs::read_to_string(default_hangar.join("hooks/transport")).unwrap(),
        "http\n",
        "and under the pinned hangar home"
    );
    let pinned = format!("AINB_HANGAR_HOME='{}'", default_hangar.display());
    assert!(
        managed_commands(&settings(home.path()))
            .iter()
            .all(|(_, c)| c.starts_with(&pinned)),
        "the hooks are pinned to the daemon's home"
    );
}

/// `setup` with no `--hooks`, as a user re-running it would type.
fn setup_plain(home: &Path) -> serde_json::Value {
    let ainb_home = home.join("ainb");
    run(
        home,
        &[("AINB_HOME", &ainb_home), ("AINB_HANGAR_HOME", &ainb_home)],
        &["setup", INSTANCE, "--no-spawn", "--no-heartbeat"],
    )
}

fn settings_bytes(home: &Path) -> Vec<u8> {
    std::fs::read(home.join(".claude").join("settings.json")).unwrap()
}

/// Review of #182 (N1): when the http marker cannot be written, settings.json
/// is put back byte for byte, never swapped for a legacy set pointing at a
/// script that was never extracted.
#[test]
fn a_failed_marker_restores_the_exact_settings() {
    let home = tempfile::tempdir().unwrap();
    user_hook(home.path());
    let before = settings_bytes(home.path());
    publish_endpoint(home.path());
    // The marker's path is a directory: writing it fails.
    std::fs::create_dir_all(home.path().join("ainb/hooks/transport")).unwrap();
    let report = setup(home.path(), "http");
    assert_eq!(report["lifecycle_hooks_installed"], false);
    assert_eq!(settings_bytes(home.path()), before, "restored exactly");
    assert!(managed_commands(&settings(home.path())).is_empty());
}

/// Review of #182 (N2): a plain re-run keeps the installed transport, and the
/// one backup keeps the settings from before the first transport change.
#[test]
fn a_plain_rerun_keeps_http_and_the_first_backup() {
    let home = tempfile::tempdir().unwrap();
    user_hook(home.path());
    let original = settings_bytes(home.path());
    publish_endpoint(home.path());
    setup(home.path(), "http");
    let backup = home.path().join(".claude/settings.json.ainb.bak");
    assert_eq!(std::fs::read(&backup).unwrap(), original);

    setup_plain(home.path());
    let commands = managed_commands(&settings(home.path()));
    assert!(!commands.is_empty());
    assert!(
        commands.iter().all(|(_, c)| c.contains("ainb-hook.sh")),
        "still http"
    );

    setup(home.path(), "legacy");
    assert_eq!(
        std::fs::read(&backup).unwrap(),
        original,
        "never overwritten"
    );
    setup_plain(home.path());
    assert!(
        managed_commands(&settings(home.path()))
            .iter()
            .all(|(_, c)| c.contains("notify.sh")),
        "a plain re-run keeps legacy too"
    );
}

/// Review of #182 (N3): an endpoint left by a daemon that is gone refuses http.
#[test]
fn a_dead_daemons_endpoint_refuses_http() {
    let home = tempfile::tempdir().unwrap();
    user_hook(home.path());
    let before = settings_bytes(home.path());
    publish_endpoint(home.path());
    let dead = {
        let mut child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    };
    let dir = home.path().join("ainb/hangar");
    let text = std::fs::read_to_string(dir.join("hook-endpoint.env")).unwrap();
    let live = format!("AINB_HOOK_PID={}", std::process::id());
    std::fs::write(
        dir.join("hook-endpoint.env"),
        text.replace(&live, &format!("AINB_HOOK_PID={dead}")),
    )
    .unwrap();
    let report = setup(home.path(), "http");
    assert_eq!(report["lifecycle_hooks_installed"], false);
    assert_eq!(settings_bytes(home.path()), before);
}
