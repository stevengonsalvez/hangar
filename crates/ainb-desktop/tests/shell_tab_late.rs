//! A shell the daemon was still making when it answered gets its tab later,
//! without another press: `shell/create` answers `SPAWN_STARTED` (tmux did
//! not answer in time), the shell appears afterwards, and the host's one late
//! look (`shell_tab::restore_after`) attaches it.
//!
//! The daemon is a fake on a scratch unix socket; the shell is a real tmux
//! session on a private server (`tmux -S` under this test's own directory),
//! ended by its exact name when the test is done, so the server exits on its
//! own. Nothing here touches any other tmux server.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ainb_desktop::shell_tab::{self, MAY_STILL_OPEN};
use ainb_desktop::terminal::{TabEvents, TabState, TabTarget, TabsView, Terminals, Tmux};
use ainb_hangar_client::DaemonClient;
use ainb_hangar_proto::spawn::{REPO_NOT_REGISTERED, SPAWN_STARTED};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

const SHELL: &str = "ainb-dsh-0123abcd";
const DIR: &str = "/code/app";

struct Quiet;

impl TabEvents for Quiet {
    fn tabs(&self, _: TabsView) {}
    fn toast(&self, _: String) {}
}

/// What the fake daemon answers `shell/create` with, and the shells its
/// `shell/list` names: those tmux has actually made by then.
#[derive(Clone)]
struct Daemon {
    create_error: Value,
    shells: Arc<Mutex<Vec<String>>>,
    lists: Arc<Mutex<usize>>,
}

async fn read_frame(reader: &mut BufReader<OwnedReadHalf>) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            let mut body = vec![0_u8; length?];
            reader.read_exact(&mut body).await.ok()?;
            return serde_json::from_slice(&body).ok();
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("Content-Length") {
                length = value.trim().parse().ok();
            }
        }
    }
}

async fn send(writer: &mut OwnedWriteHalf, value: &Value) {
    let body = serde_json::to_vec(value).expect("json");
    let head = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(head.as_bytes()).await.expect("head");
    writer.write_all(&body).await.expect("body");
}

async fn serve(stream: tokio::net::UnixStream, daemon: Daemon) {
    let (read_half, mut writer) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    while let Some(request) = read_frame(&mut reader).await {
        let id = request["id"].clone();
        let mut reply = match request["method"].as_str().unwrap_or_default() {
            "auth/hello" => json!({"result": {}}),
            "shell/create" => json!({"error": daemon.create_error}),
            "shell/list" => {
                *daemon.lists.lock().expect("lists") += 1;
                let shells: Vec<Value> = daemon
                    .shells
                    .lock()
                    .expect("shells")
                    .iter()
                    .map(|name| json!({"tmux_session_name": name, "worktree_path": DIR}))
                    .collect();
                json!({"result": {"shells": shells}})
            }
            other => {
                json!({"error": {"code": -32601, "message": format!("unknown method: {other}")}})
            }
        };
        reply["id"] = id;
        reply["jsonrpc"] = json!("2.0");
        send(&mut writer, &reply).await;
    }
}

fn listen(socket: &Path, daemon: Daemon) -> DaemonClient {
    let listener = UnixListener::bind(socket).expect("bind the fake daemon");
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve(stream, daemon.clone()));
        }
    });
    DaemonClient::with_parts(socket.to_path_buf(), "t".into())
}

/// A tmux command on the private server at `socket`.
fn tmux(socket: &Path) -> Command {
    let mut command = Command::new("tmux");
    command.env_remove("TMUX").arg("-S").arg(socket);
    command
}

/// The shell sessions this test made, ended by exact name when it is done.
struct Sessions {
    socket: PathBuf,
    names: Vec<String>,
}

impl Sessions {
    fn new(socket: &Path) -> Self {
        Self {
            socket: socket.to_path_buf(),
            names: Vec::new(),
        }
    }

    /// Make the shell `name`, as tmux does for the daemon.
    fn make(&mut self, name: &str) {
        self.names.push(name.to_string());
        let made = tmux(&self.socket)
            .args(["-f", "/dev/null", "new-session", "-d", "-s", name])
            .status()
            .is_ok_and(|status| status.success());
        assert!(made, "tmux made {name}");
    }
}

impl Drop for Sessions {
    fn drop(&mut self) {
        for name in &self.names {
            let _ = tmux(&self.socket)
                .args(["kill-session", "-t", &format!("={name}")])
                .stderr(Stdio::null())
                .status();
        }
    }
}

fn started(detail: &str) -> Value {
    json!({"code": SPAWN_STARTED, "message": detail})
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shell_that_appears_after_a_started_answer_gets_its_tab_unasked() {
    // Under /tmp: a unix socket path must stay short.
    let scratch = tempfile::Builder::new().prefix("dshl").tempdir_in("/tmp").expect("scratch");
    let tmux_socket = scratch.path().join("tmux");
    let daemon = Daemon {
        create_error: started(&format!(
            "tmux did not answer within 10s; the shell may still appear as {SHELL}"
        )),
        shells: Arc::default(),
        lists: Arc::default(),
    };
    let client = listen(&scratch.path().join("hangar.sock"), daemon.clone());
    let program = ainb_desktop::terminal::find_tmux().expect("tmux on PATH");
    let (reports, _reports_rx) = std::sync::mpsc::channel();
    let terminals = Terminals::new(
        Tmux::new(program).on_socket(tmux_socket.clone()),
        Quiet,
        reports,
    );

    // The press: the daemon says tmux started but did not finish.
    let failed = shell_tab::open_tab(&client, &terminals, DIR)
        .await
        .expect_err("the daemon did not finish the shell");
    assert!(failed.may_still_open, "a started shell may still appear");
    assert!(failed.report.is_none(), "nothing tried to attach");
    assert!(
        failed.message.starts_with(MAY_STILL_OPEN),
        "{}",
        failed.message
    );
    assert!(terminals.view().tabs.is_empty(), "no tab yet");

    // The host's one late look, scheduled now; tmux finishes the shell
    // while it waits.
    let late = {
        let client = client.clone();
        let terminals = terminals.clone();
        tokio::spawn(async move {
            shell_tab::restore_after(Duration::from_secs(3), &client, &terminals).await
        })
    };
    let mut sessions = Sessions::new(&tmux_socket);
    sessions.make(SHELL);
    daemon.shells.lock().expect("shells").push(SHELL.into());

    let (listed, failures) = late.await.expect("the late look ran").expect("listed");
    assert_eq!((listed, failures.len()), (1, 0));
    assert_eq!(
        *daemon.lists.lock().expect("lists"),
        1,
        "one look, not a loop"
    );
    let view = terminals.view();
    assert_eq!(view.tabs.len(), 1, "the late shell has its tab");
    assert_eq!(
        view.tabs[0].target,
        TabTarget::Shell {
            tmux: SHELL.into(),
            dir: DIR.into()
        }
    );
    assert_eq!(view.tabs[0].state, TabState::Attached);

    // Looking again leaves a shell that has its tab alone: no second tab.
    let (listed, failures) = shell_tab::restore(&client, &terminals).await.expect("listed");
    assert_eq!(
        (listed, failures.len(), terminals.view().tabs.len()),
        (1, 0, 1)
    );

    terminals.close(SHELL);
    drop(sessions);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_late_look_leaves_a_detached_shell_tab_detached() {
    use ainb_desktop::terminal::MAX_ATTACHED_TABS;
    let scratch = tempfile::Builder::new().prefix("dshd").tempdir_in("/tmp").expect("scratch");
    let tmux_socket = scratch.path().join("tmux");
    let daemon = Daemon {
        create_error: started("m"),
        shells: Arc::default(),
        lists: Arc::default(),
    };
    let client = listen(&scratch.path().join("hangar.sock"), daemon.clone());
    let program = ainb_desktop::terminal::find_tmux().expect("tmux on PATH");
    let (reports, reports_rx) = std::sync::mpsc::channel();
    let terminals = Terminals::new(
        Tmux::new(program).on_socket(tmux_socket.clone()),
        Quiet,
        reports,
    );
    // One shell more than stay attached: restoring them all detaches one.
    let mut sessions = Sessions::new(&tmux_socket);
    let names: Vec<String> = (0..=MAX_ATTACHED_TABS).map(|n| format!("ainb-dsh-{n:08x}")).collect();
    for name in &names {
        sessions.make(name);
    }
    daemon.shells.lock().expect("shells").extend(names.iter().cloned());
    shell_tab::restore(&client, &terminals).await.expect("listed");
    let states = || -> Vec<(String, TabState)> {
        terminals.view().tabs.into_iter().map(|tab| (tab.key, tab.state)).collect()
    };
    let before = states();
    assert_eq!(before.len(), names.len());
    assert_eq!(
        before.iter().filter(|(_, state)| *state == TabState::Detached).count(),
        1,
        "{before:?}"
    );

    let _ = reports_rx.try_iter().count();

    // The late look after another press: every shell has its tab, so it
    // reattaches none. Reattaching the detached one would detach another
    // for the cap, and so on down the strip: the same states at the end,
    // but a detach report (and toast) for each.
    shell_tab::restore(&client, &terminals).await.expect("listed");
    assert_eq!(states(), before);
    assert_eq!(
        reports_rx.try_iter().count(),
        0,
        "no tab was reattached or detached"
    );

    for name in &names {
        terminals.close(name);
    }
    drop(sessions);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_open_made_nothing_and_is_not_looked_for_again() {
    let scratch = tempfile::Builder::new().prefix("dshr").tempdir_in("/tmp").expect("scratch");
    let daemon = Daemon {
        create_error: json!({"code": REPO_NOT_REGISTERED, "message": "not a registered folder"}),
        shells: Arc::default(),
        lists: Arc::default(),
    };
    let client = listen(&scratch.path().join("hangar.sock"), daemon);
    let (reports, _reports_rx) = std::sync::mpsc::channel();
    let terminals = Terminals::new(
        Tmux::new(PathBuf::from("/nonexistent/tmux")),
        Quiet,
        reports,
    );
    let failed = shell_tab::open_tab(&client, &terminals, DIR).await.expect_err("refused");
    assert!(!failed.may_still_open, "{}", failed.message);
    assert!(failed.message.contains("Add project"), "{}", failed.message);
}
