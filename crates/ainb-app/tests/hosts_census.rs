#![allow(missing_docs)]

// ABOUTME: R1-11 against real daemons. Two daemons on two homes, each with its
// own minted HostId and its own sessions, are read through their own sockets
// and folded into one census: both `covered`; a row cap shares the rows round
// robin and marks the cut host `omitted_by_cap`; a killed daemon turns
// `unreachable{since}` with its last contact, and its rows leave the list.
//
// No WS leg is needed: the census only needs one client per host, and the
// unix socket of a second home is exactly that.

use std::time::{Duration, Instant};

use ainb_app::hosts::{
    CarrierKind, HostCoverage, HostId, HostListing, HostRegistry, Reachability, fold,
    listing_from_read,
};
use ainb_hangar_client::DaemonClient;
use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::daemon_identity::DaemonIdentityRepo;

/// One daemon: its home, its client, its minted id and the serve task.
struct Daemon {
    _home: tempfile::TempDir,
    client: DaemonClient,
    host_id: HostId,
    serve: tokio::task::JoinHandle<()>,
}

/// Boot a real listener over a fresh home, mint its identity FIRST (as
/// `boot()` does), then seed `sessions` Claude sessions through the real hook
/// reducer so every row is stamped with that identity.
async fn boot_box(sessions: usize, tag: &str) -> Daemon {
    let home = tempfile::tempdir().expect("home");
    let store = Store::open_in(home.path()).await.expect("store");
    let identity = DaemonIdentityRepo::mint_or_read(
        store.pool(),
        &ainb_hangar_core::idgen::SystemIdGen,
        &ainb_hangar_core::clock::SystemClock,
    )
    .await
    .expect("mint the host id")
    .identity;
    let broker = EventBroker::new();
    for n in 0..sessions {
        let session_id = format!("{tag}-{n:02}");
        let cwd = format!("/w/{tag}");
        let payload = serde_json::json!({
            "session_id": session_id,
            "cwd": cwd,
            "payload": {
                "session_id": session_id,
                "cwd": cwd,
                "hook_event_name": "UserPromptSubmit",
            },
        });
        ainb_hangar_daemon::fleet::apply_hook_with_attention(
            store.pool(),
            &broker.sink(),
            ainb_hangar_daemon::fleet::HookObservation {
                event_id: format!("e-{session_id}"),
                provider: "claude",
                provider_session_id: &session_id,
                cwd: &cwd,
                event_type: "UserPromptSubmit",
                payload: &payload,
                observed_at: 1_700_000_000_000 + i64::try_from(n).unwrap(),
                transcript_model: None,
            },
            None,
        )
        .await
        .expect("seed a session");
    }
    let token_path = rpc::auth::ensure_socket_token(store.pool(), home.path())
        .await
        .expect("socket token");
    let token = std::fs::read_to_string(token_path).expect("read the token");
    let socket = rpc::socket_path_in(home.path());
    let listener = rpc::bind(&socket).expect("bind");
    let health = DaemonHealth {
        socket_path: socket.to_string_lossy().into_owned(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "0.1.0".into(),
        stats: std::sync::Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    };
    let serve = tokio::spawn(rpc::serve(listener, store.pool().clone(), health, broker));
    let client = DaemonClient::with_parts(socket, token.trim().to_string());
    let hello = client.hello().await.expect("hello");
    assert_eq!(
        hello.host_id.as_deref(),
        Some(identity.host_id.as_str()),
        "the daemon names the id it minted"
    );
    Daemon {
        _home: home,
        client,
        host_id: HostId::parse(&identity.host_id).expect("a minted ULID"),
        serve,
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn two_daemons_fold_into_one_census_and_a_killed_one_turns_unreachable() {
    let a = boot_box(3, "a").await;
    let b = boot_box(2, "b").await;
    assert_ne!(a.host_id, b.host_id, "two homes mint two hosts");

    let mut registry = HostRegistry::new();
    registry.set_local(a.host_id.clone(), 1_000).unwrap();
    registry.upsert_remote(b.host_id.clone(), 1_000).unwrap();

    // Both reachable: every row is in, under its own host.
    let la = listing_from_read(
        &mut registry,
        &a.host_id,
        a.client.fleet_roster_status().await,
        CarrierKind::SshL,
        2_000,
    );
    let lb = listing_from_read(
        &mut registry,
        &b.host_id,
        b.client.fleet_roster_status().await,
        CarrierKind::Tailnet,
        2_000,
    );
    assert!(
        matches!(&la, HostListing::Fresh { rows, .. } if rows.len() == 3),
        "{la:?}"
    );
    assert!(
        matches!(&lb, HostListing::Fresh { rows, .. } if rows.len() == 2),
        "{lb:?}"
    );
    assert_eq!(
        registry.get(&b.host_id).unwrap().reachability,
        Reachability::Reachable {
            carrier: CarrierKind::Tailnet
        }
    );
    let census = fold(
        vec![
            (a.host_id.clone(), la.clone()),
            (b.host_id.clone(), lb.clone()),
        ],
        50,
    );
    assert_eq!(census.rows.len(), 5);
    assert_eq!(census.coverage_of(&a.host_id), Some(HostCoverage::Covered));
    assert_eq!(census.coverage_of(&b.host_id), Some(HostCoverage::Covered));
    for row in &census.rows {
        assert_eq!(row.row.status.host_id, row.host_id.as_str(), "{row:?}");
        let tag = if row.host_id == a.host_id { "a-" } else { "b-" };
        assert!(row.row.status.session_key.contains(tag), "{row:?}");
        assert!(!row.stale);
    }

    // A cap of 4 is shared round robin (a1 b1, then a2 b2): B keeps both of
    // its rows and only the busy host A is cut, by one.
    let capped = fold(vec![(a.host_id.clone(), la), (b.host_id.clone(), lb)], 4);
    assert_eq!(capped.rows.len(), 4);
    assert_eq!(
        capped.coverage_of(&a.host_id),
        Some(HostCoverage::OmittedByCap { omitted: 1 })
    );
    assert_eq!(capped.coverage_of(&b.host_id), Some(HostCoverage::Covered));
    assert_eq!(
        capped.rows.iter().filter(|r| r.host_id == b.host_id).count(),
        2,
        "round robin: the quiet host keeps every row"
    );

    // Kill B's daemon. Its next read fails: unreachable since the last
    // contact (2_000), not since the failed read, and no B row is listed.
    b.serve.abort();
    let _ = b.serve.await;
    let mut lb = listing_from_read(
        &mut registry,
        &b.host_id,
        b.client.fleet_roster_status().await,
        CarrierKind::Tailnet,
        9_000,
    );
    for _ in 0..20 {
        if matches!(lb, HostListing::Unreachable { .. }) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        lb = listing_from_read(
            &mut registry,
            &b.host_id,
            b.client.fleet_roster_status().await,
            CarrierKind::Tailnet,
            9_000,
        );
    }
    assert_eq!(lb, HostListing::Unreachable { since_ms: 2_000 });
    assert_eq!(
        registry.get(&b.host_id).unwrap().reachability,
        Reachability::Unreachable { since_ms: 2_000 }
    );
    let la = listing_from_read(
        &mut registry,
        &a.host_id,
        a.client.fleet_roster_status().await,
        CarrierKind::SshL,
        9_000,
    );
    let census = fold(vec![(a.host_id.clone(), la), (b.host_id.clone(), lb)], 50);
    assert_eq!(census.rows.len(), 3);
    assert!(census.rows.iter().all(|r| r.host_id == a.host_id));
    assert_eq!(
        census.coverage_of(&b.host_id),
        Some(HostCoverage::Unreachable { since_ms: 2_000 })
    );
    a.serve.abort();
}

/// A client pointed at the wrong daemon: its rows name another host, so the
/// listing is refused rather than filed under the host the caller asked for.
#[tokio::test]
async fn rows_from_another_host_are_never_filed_under_this_one() {
    let a = boot_box(1, "a").await;
    let expected = HostId::parse("01K5A0000000000000000ZZZZZ").unwrap();
    let mut registry = HostRegistry::new();
    registry.upsert_remote(expected.clone(), 5).unwrap();
    let listing = listing_from_read(
        &mut registry,
        &expected,
        a.client.fleet_roster_status().await,
        CarrierKind::Lan,
        10,
    );
    assert_eq!(listing, HostListing::Unreachable { since_ms: 5 });
    assert!(matches!(
        registry.get(&expected).unwrap().reachability,
        Reachability::Unreachable { .. }
    ));
    a.serve.abort();
}

/// The local host must be keyed by its minted id: the daemon stamps its rows
/// with that id, so a local host still keyed `local` refuses its own rows,
/// and re-keying it (as a surface does once hello names the id) fixes that.
#[tokio::test]
async fn the_local_host_keyed_local_refuses_its_own_rows_until_re_keyed() {
    let a = boot_box(2, "a").await;
    let mut registry = HostRegistry::new();
    registry.set_local(HostId::local(), 1).unwrap();
    let refused = listing_from_read(
        &mut registry,
        &HostId::local(),
        a.client.fleet_roster_status().await,
        CarrierKind::SshL,
        2,
    );
    assert!(
        matches!(refused, HostListing::Unreachable { .. }),
        "{refused:?}"
    );

    registry.set_local(a.host_id.clone(), 3).unwrap();
    let listed = listing_from_read(
        &mut registry,
        &a.host_id,
        a.client.fleet_roster_status().await,
        CarrierKind::SshL,
        4,
    );
    assert!(
        matches!(&listed, HostListing::Fresh { rows, .. } if rows.len() == 2),
        "{listed:?}"
    );
    assert_eq!(registry.local().map(|h| &h.host_id), Some(&a.host_id));
    a.serve.abort();
}
