//! A send addressed to `session:window.pane` lands in that pane, not in the
//! session's active one (#132): a split ainb session would otherwise take the
//! agent's answer into whichever pane the person last used.
//!
//! Runs against a private tmux server: `TMUX_TMPDIR` names a directory of this
//! test's own, so no session of anyone else's is touched, and the one session
//! it creates is killed by its exact name. Skipped where tmux is not installed.

use std::process::Command;
use std::time::{Duration, Instant};

const TEST: &str = "a_send_to_an_exact_pane_lands_there_and_not_in_the_active_pane";

fn tmux(args: &[&str]) -> Option<String> {
    let output = Command::new("tmux").args(args).output().ok()?;
    if !output.status.success() {
        eprintln!(
            "tmux {args:?}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[test]
fn a_send_to_an_exact_pane_lands_there_and_not_in_the_active_pane() {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("tmux is not installed: nothing to prove here");
        return;
    }
    // The private server: every tmux command in the child process, the
    // crate's own included, resolves its socket under `TMUX_TMPDIR`. The
    // workspace forbids unsafe code, and setting a variable in a running
    // process is that, so the test runs its body in a child of its own with
    // the environment it needs.
    if std::env::var_os("EXACT_PANE_CHILD").is_none() {
        let dir = tempfile::tempdir().unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env("EXACT_PANE_CHILD", "1")
            .env("TMUX_TMPDIR", dir.path())
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .status()
            .expect("the test binary runs itself");
        assert!(status.success(), "the child run failed: {status}");
        return;
    }
    let dir_shown = std::env::var("TMUX_TMPDIR").unwrap_or_default();
    let name = format!("exact-pane-{}", std::process::id());
    let session = format!("={name}");

    // The first pane runs `cat`, echoing every line it reads; the split is
    // the ACTIVE pane from here on. Indices come from tmux itself: a person's
    // config may start windows and panes at 1.
    assert!(
        tmux(&[
            "new-session",
            "-d",
            "-s",
            &name,
            "-x",
            "80",
            "-y",
            "24",
            "cat"
        ])
        .is_some()
    );
    let window = tmux(&["display-message", "-p", "-t", &name, "#{window_index}"])
        .unwrap()
        .trim()
        .to_string();
    let first = tmux(&["display-message", "-p", "-t", &name, "#{pane_index}"])
        .unwrap()
        .trim()
        .to_string();
    assert!(
        tmux(&["split-window", "-t", &name, "cat"]).is_some(),
        "the split"
    );
    // Every pane by its id, which no config renumbers, with its index and
    // whether it is active.
    let panes = tmux(&[
        "list-panes",
        "-t",
        &name,
        "-F",
        "#{pane_id} #{pane_index} #{pane_active}",
    ])
    .unwrap();
    let mut listed = panes.lines().map(|line| {
        let mut parts = line.split(' ');
        (
            parts.next().unwrap().to_string(),
            parts.next().unwrap().to_string(),
            parts.next() == Some("1"),
        )
    });
    let (first_id, first, _) =
        listed.clone().find(|(_, index, _)| *index == first).expect("the first pane");
    let (active_id, active, _) = listed.find(|(_, _, active)| *active).expect("an active pane");
    assert_ne!(
        active, first,
        "the split is the active pane, the first is not:\n{panes}"
    );
    eprintln!("panes:\n{panes}first={first_id} ({first}) active={active_id} ({active})");

    let target = format!("{name}:{window}.{first}");
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    // A digit: the verified picker send takes an option's digit, Enter and
    // the arrows, nothing else, and `cat` echoes it back on its own line.
    let text = "7";
    runtime
        .block_on(ainb_fleet_core::send::tmux_send_picker_key(&target, text))
        .expect("send-keys to the exact pane");
    runtime
        .block_on(ainb_fleet_core::send::tmux_send_picker_key(
            &target, "Enter",
        ))
        .unwrap();

    let capture = |id: &str| tmux(&["capture-pane", "-p", "-t", id]).unwrap_or_default();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !capture(&first_id).contains(text) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let (targeted, other) = (capture(&first_id), capture(&active_id));
    eprintln!("killing tmux session {name} on {dir_shown}");
    let _ = tmux(&["kill-session", "-t", &session]);
    assert!(
        targeted.contains(text),
        "the target pane read the text:\n{targeted}"
    );
    assert!(!other.contains(text), "the active pane did not:\n{other}");
}
