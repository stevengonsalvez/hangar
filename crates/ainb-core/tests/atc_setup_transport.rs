//! `ainb fleet atc setup --hooks=http|legacy` end to end, in a temp `$HOME`.
//!
//! Proves the operator-visible migration: http is refused while no daemon
//! publishes a hook endpoint, then rewrites the managed entries in place to the
//! 15-event set once one does, and `--hooks=legacy` puts the 30-event set back.

use std::path::{Path, PathBuf};
use std::process::Command;

use ainb_hangar_proto::hooks::{CLAUDE_HOOK_EVENTS, HookEndpoint, render_headers_file};

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

fn setup(home: &Path, transport: &str) -> serde_json::Value {
    let ainb_home = home.join("ainb");
    let out = Command::new(ainb_bin())
        .env("HOME", home)
        .env("AINB_HOME", &ainb_home)
        .env("AINB_HANGAR_HOME", &ainb_home)
        .env("AINB_BIN", ainb_bin())
        .args([
            "--format",
            "json",
            "fleet",
            "atc",
            "setup",
            "tower",
            "--no-spawn",
            "--no-heartbeat",
            "--hooks",
            transport,
        ])
        .output()
        .expect("invoke ainb");
    assert!(
        out.status.success(),
        "setup failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("setup prints JSON")
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
    let dir = home.join("ainb").join("hangar");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    std::fs::write(dir.join("hook-headers"), render_headers_file("t")).unwrap();
    let endpoint = HookEndpoint {
        port: 45678,
        version: 1,
        pid: 1,
        headers_path: dir.join("hook-headers"),
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
    assert_eq!(
        std::fs::read_to_string(home.path().join("ainb/hooks/transport")).unwrap(),
        "legacy\n"
    );
}
