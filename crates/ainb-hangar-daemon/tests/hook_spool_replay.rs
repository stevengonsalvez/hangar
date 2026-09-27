//! The real `ainb-hook.sh` against the real listener and ingest sink.
//!
//! With no daemon the script spools status events (never a hold); when the
//! listener starts it replays them once, by the script-minted event id, and
//! live calls then flow straight through.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use ainb_hangar_daemon::attention_ingest::AttentionIngest;
use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::hook_ingress::{self, IngestSink};
use ainb_hangar_proto::hooks::SPOOL_DIR_NAME;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::fleet::FleetRepo;
use std::io::Write as _;

fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/ainb-hooks/hooks/ainb-hook.sh")
        .canonicalize()
        .unwrap()
}

/// Run the script like a managed Claude entry; return its stdout.
fn fire(home: &Path, event: &str, payload: &str) -> String {
    let mut child = Command::new("sh")
        .arg(script())
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap())
        .env("HOME", home)
        .env("AINB_HANGAR_HOME", home)
        .env("AINB_AGENT", "claude")
        .env("AINB_HOOK_EVENT", event)
        .env("AINB_MANAGED", "atc")
        .env("AINB_PANE_KEY", "v1:replay-1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap()
}

fn spool_lines(home: &Path) -> Vec<String> {
    let dir = home.join("hangar").join(SPOOL_DIR_NAME);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .flat_map(|e| {
            std::fs::read_to_string(e.path())
                .unwrap()
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

async fn provider_events(store: &Store, session: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM fleet_provider_event WHERE provider_session_id = ?")
        .bind(session)
        .fetch_one(store.pool())
        .await
        .unwrap()
}

async fn start(home: &Path, store: &Store) -> hook_ingress::Running {
    let broker = EventBroker::new();
    let ingest = AttentionIngest::new(
        store.pool().clone(),
        broker.sink(),
        home.join("events.jsonl"),
        home.join("cursor"),
    );
    let sink = IngestSink::new(
        ingest,
        home.to_path_buf(),
        store.pool().clone(),
        broker.sink(),
    );
    hook_ingress::start(home, Arc::new(sink)).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spooled_status_replays_once_and_holds_never_do() {
    let home = tempfile::tempdir().unwrap();
    let session = format!("replay-{}", std::process::id());
    let payload = |event: &str, extra: &str| {
        format!(
            r#"{{"hook_event_name":"{event}","session_id":"{session}","cwd":"/tmp/replay","transcript_path":""{extra}}}"#
        )
    };

    // No daemon: two status events spool, the hold does not.
    assert_eq!(
        fire(home.path(), "SessionStart", &payload("SessionStart", "")),
        "{}\n"
    );
    assert_eq!(
        fire(
            home.path(),
            "PermissionRequest",
            &payload(
                "PermissionRequest",
                r#","tool_name":"Bash","tool_input":{"command":"ls"}"#
            )
        ),
        "{}\n",
        "no daemon: the agent prompts itself"
    );
    assert_eq!(fire(home.path(), "Stop", &payload("Stop", "")), "{}\n");
    let spooled = spool_lines(home.path());
    assert_eq!(spooled.len(), 2, "{spooled:?}");
    assert!(spooled.iter().all(|l| !l.contains("PermissionRequest")));

    // The same event spooled twice (recorded, then spooled after a 503).
    let dir = home.path().join("hangar").join(SPOOL_DIR_NAME);
    let file = std::fs::read_dir(&dir).unwrap().next().unwrap().unwrap().path();
    let mut f = std::fs::OpenOptions::new().append(true).open(&file).unwrap();
    writeln!(f, "{}", spooled[0]).unwrap();
    drop(f);

    // The daemon starts: the spool drains, once.
    let store = Store::open_in(home.path()).await.unwrap();
    let running = start(home.path(), &store).await;
    for _ in 0..100 {
        if spool_lines(home.path()).is_empty() && provider_events(&store, &session).await >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(spool_lines(home.path()).is_empty(), "the spool was drained");
    assert_eq!(
        provider_events(&store, &session).await,
        2,
        "the duplicate was recorded once"
    );
    let row = FleetRepo::get_session(store.pool(), &format!("claude:{session}"))
        .await
        .unwrap();
    assert!(
        row.is_some(),
        "the replayed events reduced into a Fleet row"
    );

    // Live: the script now reaches the daemon and nothing spools.
    assert_eq!(
        fire(home.path(), "Notification", &payload("Notification", "")),
        "{}\n"
    );
    for _ in 0..100 {
        if provider_events(&store, &session).await == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(provider_events(&store, &session).await, 3);
    assert!(spool_lines(home.path()).is_empty());
    drop(running);
}
