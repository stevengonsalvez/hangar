//! `ainb-hook.sh`, the HTTP-transport hook script, driven against an in-test
//! listener that stands in for the hangar daemon.
//!
//! Each test builds a temp hangar home with the two files the daemon publishes
//! (`hangar/hook-endpoint.env` and the 0600 `hangar/hook-headers`), runs the
//! script under POSIX `sh` exactly as a managed hook entry does, and asserts on
//! what the agent would read from stdout and what the listener received.
//! Needs `sh` and `curl` on PATH, as the script does.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use ainb_hangar_proto::hooks::{
    ENDPOINT_FILE_NAME, HEADERS_FILE_NAME, HookEndpoint, SPOOL_DIR_NAME, render_headers_file,
};

const TOKEN: &str = "5e0c8a51-3d7b-4d0e-9c1a-7b4e2f6a9d10";
const ALLOW: &str = r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}"#;

fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/ainb-hooks/hooks/ainb-hook.sh")
        .canonicalize()
        .unwrap()
}

/// One request as the listener saw it.
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// How the fake daemon answers.
#[derive(Clone, Copy)]
enum Reply {
    NoContent,
    Allow,
    /// Accept, read, then say nothing for this long.
    Stall(Duration),
}

struct Fake {
    port: u16,
    seen: mpsc::Receiver<Seen>,
}

fn fake(reply: Reply) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let tx = tx.clone();
            thread::spawn(move || serve(stream, reply, &tx));
        }
    });
    Fake { port, seen: rx }
}

fn serve(mut stream: TcpStream, reply: Reply, tx: &mpsc::Sender<Seen>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let path = line.split_whitespace().nth(1).unwrap_or_default().to_string();
    let mut headers = Vec::new();
    let mut len = 0;
    loop {
        let mut h = String::new();
        reader.read_line(&mut h).unwrap();
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                len = v.trim().parse().unwrap();
            }
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let mut body = vec![0; len];
    reader.read_exact(&mut body).unwrap();
    let _ = tx.send(Seen {
        path,
        headers,
        body: String::from_utf8(body).unwrap(),
    });
    let response = match reply {
        Reply::NoContent => "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n".to_string(),
        Reply::Allow => format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{ALLOW}",
            ALLOW.len()
        ),
        Reply::Stall(d) => {
            thread::sleep(d);
            return;
        }
    };
    let _ = stream.write_all(response.as_bytes());
}

/// A hangar home publishing `port`, as the daemon does.
fn home_for(port: u16) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    publish(home.path(), port);
    home
}

fn publish(home: &Path, port: u16) {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = home.join("hangar");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    let headers = dir.join(HEADERS_FILE_NAME);
    std::fs::write(&headers, render_headers_file(TOKEN)).unwrap();
    std::fs::set_permissions(&headers, std::fs::Permissions::from_mode(0o600)).unwrap();
    let endpoint = HookEndpoint {
        port,
        version: 1,
        pid: 1,
        headers_path: headers,
    };
    std::fs::write(dir.join(ENDPOINT_FILE_NAME), endpoint.render_env_file()).unwrap();
}

/// Run the script like a managed entry. Returns (stdout, elapsed).
fn fire(home: &Path, event: &str, payload: &str, env: &[(&str, &str)]) -> (String, Duration) {
    let mut cmd = Command::new("sh");
    cmd.arg(script())
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap())
        .env("HOME", home)
        .env("AINB_HANGAR_HOME", home)
        .env("AINB_AGENT", "claude")
        .env("AINB_HOOK_EVENT", event)
        .env("AINB_MANAGED", "atc")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let started = Instant::now();
    let mut child = cmd.spawn().unwrap();
    child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "the hook always exits 0");
    (String::from_utf8(out.stdout).unwrap(), started.elapsed())
}

fn spool_lines(home: &Path) -> Vec<(String, String)> {
    let dir = home.join("hangar").join(SPOOL_DIR_NAME);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries {
        let e = e.unwrap();
        let name = e.file_name().to_string_lossy().into_owned();
        for line in std::fs::read_to_string(e.path()).unwrap().lines() {
            out.push((name.clone(), line.to_string()));
        }
    }
    out
}

#[test]
fn a_status_event_posts_with_token_and_pane_headers_and_prints_empty() {
    let f = fake(Reply::NoContent);
    let home = home_for(f.port);
    let payload = r#"{"hook_event_name":"Notification","session_id":"s1"}"#;
    let (out, _) = fire(
        home.path(),
        "Notification",
        payload,
        &[("AINB_PANE_KEY", "v1:abc-1"), ("TMUX_PANE", "%4")],
    );
    assert_eq!(out, "{}\n");
    let seen = f.seen.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(seen.path, "/hook/claude");
    assert_eq!(seen.header("X-Ainb-Hook-Token"), Some(TOKEN));
    assert_eq!(seen.header("X-Ainb-Pane-Key"), Some("v1:abc-1"));
    assert_eq!(seen.header("X-Ainb-Tmux-Pane"), Some("%4"));
    assert_eq!(seen.header("Content-Type"), Some("application/json"));
    assert_eq!(seen.header("Expect"), None, "no 100-continue stall");
    assert_eq!(seen.body, payload);
    assert!(spool_lines(home.path()).is_empty());
}

#[test]
fn a_permission_request_holds_and_prints_the_daemons_decision() {
    let f = fake(Reply::Allow);
    let home = home_for(f.port);
    let (out, _) = fire(
        home.path(),
        "PermissionRequest",
        r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash"}"#,
        &[],
    );
    assert_eq!(out.trim_end(), ALLOW);
    assert_eq!(
        f.seen.recv_timeout(Duration::from_secs(5)).unwrap().path,
        "/hook/claude/hold"
    );
}

#[test]
fn only_ask_user_question_holds_among_pre_tool_use() {
    let f = fake(Reply::Allow);
    let home = home_for(f.port);
    let (out, _) = fire(
        home.path(),
        "PreToolUse",
        r#"{"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_input":{}}"#,
        &[],
    );
    assert_eq!(out.trim_end(), ALLOW);
    assert_eq!(f.seen.recv().unwrap().path, "/hook/claude/hold");

    let (out, _) = fire(
        home.path(),
        "PreToolUse",
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash"}"#,
        &[],
    );
    assert_eq!(out, "{}\n", "a non-ask tool never prints the daemon's body");
    assert_eq!(f.seen.recv().unwrap().path, "/hook/claude");
}

#[test]
fn a_hold_that_gets_no_decision_prints_empty_so_the_agent_prompts() {
    let f = fake(Reply::NoContent);
    let home = home_for(f.port);
    let (out, _) = fire(
        home.path(),
        "PermissionRequest",
        r#"{"hook_event_name":"PermissionRequest"}"#,
        &[],
    );
    assert_eq!(out, "{}\n");
}

#[test]
fn a_stalled_daemon_costs_a_status_hook_under_two_seconds_and_spools_it() {
    let f = fake(Reply::Stall(Duration::from_secs(10)));
    let home = home_for(f.port);
    let (out, took) = fire(
        home.path(),
        "SessionStart",
        r#"{"hook_event_name":"SessionStart","session_id":"s2"}"#,
        &[],
    );
    assert_eq!(out, "{}\n");
    assert!(took < Duration::from_millis(2500), "took {took:?}");
    let lines = spool_lines(home.path());
    assert_eq!(lines.len(), 1, "{lines:?}");
    let v: serde_json::Value = serde_json::from_str(&lines[0].1).unwrap();
    assert_eq!(v["event"], "SessionStart");
    assert_eq!(v["payload"]["session_id"], "s2");
}

#[test]
fn with_no_daemon_non_tool_events_spool_and_tool_events_do_not() {
    let home = tempfile::tempdir().unwrap(); // no endpoint file at all
    let (out, _) = fire(
        home.path(),
        "PostToolUse",
        r#"{"hook_event_name":"PostToolUse","session_id":"s3"}"#,
        &[("AINB_PANE_KEY", "v1:p1")],
    );
    assert_eq!(out, "{}\n");
    assert!(
        spool_lines(home.path()).is_empty(),
        "tool events never spool"
    );
    let (out, _) = fire(
        home.path(),
        "PermissionRequest",
        r#"{"hook_event_name":"PermissionRequest","session_id":"s3"}"#,
        &[("AINB_PANE_KEY", "v1:p1")],
    );
    assert_eq!(
        out, "{}\n",
        "a hold with no daemon falls to the agent's prompt"
    );
    let lines = spool_lines(home.path());
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].0, "v1:p1.jsonl");
}

#[test]
fn spool_names_are_sanitised() {
    let home = tempfile::tempdir().unwrap();
    fire(
        home.path(),
        "Stop",
        r#"{"hook_event_name":"Stop"}"#,
        &[("AINB_PANE_KEY", "v1:../a b/c")],
    );
    let lines = spool_lines(home.path());
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].0, "v1:___a_b_c.jsonl");
    let dir = home.path().join("hangar").join(SPOOL_DIR_NAME);
    assert_eq!(
        std::fs::read_dir(dir).unwrap().count(),
        1,
        "nothing escaped"
    );
}

#[test]
fn the_endpoint_file_is_reread_on_every_call() {
    let first = fake(Reply::NoContent);
    let second = fake(Reply::NoContent);
    let home = home_for(first.port);
    fire(home.path(), "Stop", r#"{"hook_event_name":"Stop"}"#, &[]);
    first.seen.recv_timeout(Duration::from_secs(5)).unwrap();
    publish(home.path(), second.port); // the daemon restarted
    fire(home.path(), "Stop", r#"{"hook_event_name":"Stop"}"#, &[]);
    second.seen.recv_timeout(Duration::from_secs(5)).unwrap();
}

#[test]
fn a_shell_laden_endpoint_file_runs_nothing() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("hangar");
    std::fs::create_dir_all(&dir).unwrap();
    let marker = home.path().join("pwned");
    std::fs::write(
        dir.join(ENDPOINT_FILE_NAME),
        format!(
            "AINB_HOOK_PORT=1; touch {m}\nAINB_HOOK_HEADERS=$(touch {m})\n`touch {m}`\n",
            m = marker.display()
        ),
    )
    .unwrap();
    let (out, _) = fire(
        home.path(),
        "Notification",
        r#"{"hook_event_name":"Notification"}"#,
        &[],
    );
    assert_eq!(out, "{}\n");
    assert!(!marker.exists(), "the endpoint file was executed");
    assert_eq!(spool_lines(home.path()).len(), 1, "unreachable, so spooled");
}

#[test]
fn the_token_never_appears_in_any_process_argv() {
    let f = fake(Reply::Stall(Duration::from_secs(4)));
    let home = home_for(f.port);
    let home_path = home.path().to_path_buf();
    let hook = thread::spawn(move || {
        fire(
            &home_path,
            "PermissionRequest",
            r#"{"hook_event_name":"PermissionRequest"}"#,
            &[],
        )
    });
    // The request is held open: curl is alive now.
    f.seen.recv_timeout(Duration::from_secs(5)).unwrap();
    let ps = Command::new("ps").args(["-eo", "args"]).output().unwrap();
    let ps = String::from_utf8_lossy(&ps.stdout);
    let curl: Vec<&str> = ps
        .lines()
        .filter(|l| l.contains("curl") && l.contains(&f.port.to_string()))
        .collect();
    assert_eq!(curl.len(), 1, "exactly our curl is running: {ps}");
    assert!(
        curl[0].contains("-H @"),
        "headers come from the file: {}",
        curl[0]
    );
    assert!(
        !ps.contains(TOKEN),
        "the token is visible in some process argv"
    );
    let (out, _) = hook.join().unwrap();
    assert_eq!(
        out, "{}\n",
        "a stalled hold falls back to the agent's prompt"
    );
}

#[test]
fn a_background_job_worker_is_ignored() {
    let f = fake(Reply::Allow);
    let home = home_for(f.port);
    let (out, _) = fire(
        home.path(),
        "PermissionRequest",
        r#"{"hook_event_name":"PermissionRequest"}"#,
        &[("CLAUDE_JOB_DIR", "/tmp/job")],
    );
    assert_eq!(out, "{}\n");
    assert!(f.seen.recv_timeout(Duration::from_millis(300)).is_err());
}

#[test]
fn notify_sh_stands_down_when_the_transport_is_http() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("hooks")).unwrap();
    std::fs::write(home.path().join("hooks").join("transport"), "http\n").unwrap();
    let notify = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/ainb-hooks/hooks/notify.sh")
        .canonicalize()
        .unwrap();
    let mut child = Command::new("bash")
        .arg(notify)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap())
        .env("HOME", home.path())
        .env("AINB_HANGAR_HOME", home.path())
        .env("AINB_AGENT", "claude")
        .env("AINB_NOTIFY_DISABLE_LAZY_SPAWN", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"hook_event_name":"PermissionRequest","session_id":"s"}"#)
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "{}\n");
    assert!(
        !home.path().join("notify.fallback.jsonl").exists(),
        "no second copy of the event was delivered"
    );
}
