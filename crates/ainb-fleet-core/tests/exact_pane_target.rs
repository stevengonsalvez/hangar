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

    // The resolver (#132 follow-up): the fingerprint's pane id wins and is
    // checked to be in this session, in a session created when the row was
    // raised, and still running the pid the row was raised with; an index
    // target is never trusted, so with nothing else recorded two panes
    // refuse and one pane is typed into, counted across every window. After
    // the first pane closes the second is renumbered, so its old index names
    // nothing while its id still does.
    use ainb_fleet_core::send::{PaneHint, resolve_send_target, tmux_session_exists};
    let resolve = |hint: PaneHint| runtime.block_on(resolve_send_target(&name, &hint));
    let read = |id: &str, format: &str| {
        tmux(&["display-message", "-p", "-t", id, format]).unwrap().trim().to_string()
    };
    let fingerprint = |id: &str| {
        Some(format!(
            "pane={id};pid={};session_started={}",
            read(id, "#{pane_pid}"),
            read(id, "#{session_created}")
        ))
    };
    assert!(
        runtime.block_on(tmux_session_exists(&active_id)),
        "the tmux gate answers for a pane id"
    );
    assert!(
        !runtime.block_on(tmux_session_exists("%999")),
        "and refuses a pane id nothing runs under"
    );
    assert_eq!(
        resolve(PaneHint {
            target: Some(target.clone()),
            fingerprint: fingerprint(&active_id)
        }),
        Ok(active_id.clone()),
        "the fingerprint's id outranks the index target"
    );
    assert_eq!(
        resolve(PaneHint {
            target: None,
            fingerprint: Some(format!("pane={active_id}"))
        }),
        Ok(active_id.clone()),
        "a fingerprint with no pid and no start is checked for the session alone"
    );
    let refused = resolve(PaneHint {
        target: Some(target.clone()),
        fingerprint: None,
    })
    .unwrap_err();
    assert!(
        refused.contains("2 panes") && refused.contains("not trusted"),
        "an index target alone is not typed into: {refused}"
    );
    assert!(
        resolve(PaneHint::default()).unwrap_err().contains("2 panes"),
        "two panes and nothing recorded: refused"
    );
    assert!(
        resolve(PaneHint {
            target: None,
            fingerprint: fingerprint("%999")
        })
        .unwrap_err()
        .contains("gone"),
        "a pane id nothing runs under"
    );
    let stale = resolve(PaneHint {
        target: None,
        fingerprint: Some(format!("pane={active_id};pid=1")),
    })
    .unwrap_err();
    assert!(
        stale.contains("another process"),
        "a pane that runs another pid than the row's: {stale}"
    );
    let restarted = resolve(PaneHint {
        target: None,
        fingerprint: Some(format!("pane={active_id};session_started=1")),
    })
    .unwrap_err();
    assert!(
        restarted.contains("created since"),
        "a pane id a restarted server handed out again: {restarted}"
    );
    // The pane ainb respawns keeps its id and gets a new pid: the row raised
    // by what ran there before is refused, a row raised by what runs there
    // now is delivered.
    let raised_by_first = fingerprint(&first_id);
    let pid_before = read(&first_id, "#{pane_pid}");
    assert!(
        tmux(&["respawn-pane", "-k", "-t", &first_id, "cat"]).is_some(),
        "the first pane respawns"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while read(&first_id, "#{pane_pid}") == pid_before && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let respawned = resolve(PaneHint {
        target: None,
        fingerprint: raised_by_first,
    })
    .unwrap_err();
    assert!(
        respawned.contains("another process"),
        "the respawned pane is not the agent that asked: {respawned}"
    );
    assert_eq!(
        resolve(PaneHint {
            target: None,
            fingerprint: fingerprint(&first_id)
        }),
        Ok(first_id.clone()),
        "what runs in the respawned pane now is delivered to"
    );
    // The routed send, aimed at a pane id: the tmux gate lets it through and
    // the text reaches that pane and no other. A `cat` pane is not a
    // composer, so what the verified send makes of the echo is not asserted;
    // that it was not turned away as no session is.
    let sent = "routed-7";
    let on_pane = ainb_fleet_core::types::Session {
        id: String::new(),
        provider_session_id: None,
        cwd: String::new(),
        pid: None,
        git_root: None,
        tmux_session: Some(first_id.clone()),
        workspace_name: Some(name.clone()),
        worktree_path: None,
        peer_id: None,
        bg_job_id: None,
        transcript_path: None,
        sources: vec![ainb_fleet_core::types::SessionSource::Ainb],
        summary: None,
        last_seen_ms: None,
    };
    let outcome = runtime.block_on(ainb_fleet_core::send::send(&on_pane, sent));
    eprintln!("routed send to {first_id}: {outcome:?}");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !capture(&first_id).contains(sent) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        capture(&first_id).contains(sent),
        "the routed send reached the pane it named:\n{}",
        capture(&first_id)
    );
    assert!(
        !capture(&active_id).contains(sent),
        "and not the active pane:\n{}",
        capture(&active_id)
    );
    if let Ok(ainb_fleet_core::types::SendOutcome::Failed { reason }) = &outcome {
        assert!(
            !reason.contains("no live tmux session"),
            "the tmux gate turned the pane id away: {reason}"
        );
    }
    let second_index_before = active.clone();
    assert!(
        tmux(&["kill-pane", "-t", &first_id]).is_some(),
        "the first pane closes"
    );
    let second_index_after = read(&active_id, "#{pane_index}");
    assert_ne!(
        second_index_after, second_index_before,
        "tmux renumbered the surviving pane"
    );
    assert_eq!(
        resolve(PaneHint {
            target: Some(format!("{name}:{window}.{second_index_before}")),
            fingerprint: fingerprint(&active_id)
        }),
        Ok(active_id.clone()),
        "the fingerprint still names the surviving pane after the renumbering"
    );
    assert_eq!(
        resolve(PaneHint::default()),
        Ok(active_id.clone()),
        "one pane left: typed into"
    );
    // A pane in another window of the session counts: with nothing recorded
    // the session has two panes again, whichever window is current.
    assert!(
        tmux(&["new-window", "-d", "-t", &name, "cat"]).is_some(),
        "a second window opens"
    );
    let across = resolve(PaneHint::default()).unwrap_err();
    assert!(
        across.contains("2 panes"),
        "a pane in another window is counted: {across}"
    );

    eprintln!("killing tmux session {name} on {dir_shown}");
    let _ = tmux(&["kill-session", "-t", &session]);
    assert!(
        targeted.contains(text),
        "the target pane read the text:\n{targeted}"
    );
    assert!(!other.contains(text), "the active pane did not:\n{other}");
}
