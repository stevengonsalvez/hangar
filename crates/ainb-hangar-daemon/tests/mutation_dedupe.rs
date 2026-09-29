//! The D18 gate: every mutation is deduplicated at dispatch.
//!
//! Three properties, each a failure this ledger exists to stop:
//!
//! 1. **Replay is a read.** Every method in the committed `MUTATING_METHODS`
//!    registry is dispatched twice with one op id. The first execution is
//!    `created`; the second is `replayed` and returns the first reply verbatim.
//! 2. **An op id belongs to its principal.** A second principal presenting the
//!    same op id is `rejected{op_id_foreign}`, never a second execution under
//!    somebody else's identity.
//! 3. **A different body is a different operation.** The same op id with a
//!    changed body is `rejected{already_answered_by}`, not a silent overwrite.
//!
//! The registry walk is the point: a mutating method added without a ledger
//! entry is a hole in "generic dedupe at dispatch for every mutation", and this
//! test is what turns that hole into a failure.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::rpc::{self, DaemonHealth, auth::Caller};
use ainb_hangar_proto::mutation::{ACK_KEY, MUTATING_METHODS};
use ainb_hangar_proto::{RpcId, RpcRequest};
use ainb_hangar_store::Store;

/// No handler in the registry should take anywhere near this. A method that
/// does is a method that would wedge a real client, and the test says so.
const PER_CALL: Duration = Duration::from_secs(30);

/// A host this process keeps to itself, set before any test dispatches: an
/// empty `HOME` (no registered folder, so the spawn verbs' samples are
/// refused before they run anything), a private tmux server directory with
/// no server in it, and the spawn switch unset, as a default daemon has it.
/// Every test calls this first; the values outlive the tests on purpose.
fn private_host() {
    static SET: std::sync::Once = std::sync::Once::new();
    SET.call_once(|| {
        let home = tempfile::tempdir().unwrap().keep();
        // Under /tmp: macOS caps unix socket paths at 104 bytes.
        let tmux_dir =
            std::path::PathBuf::from("/tmp").join(format!("ainb-dedupe-{}", std::process::id()));
        std::fs::create_dir_all(&tmux_dir).unwrap();
        // Edition 2021: set_var is safe, and the Once runs it before any
        // test goes on.
        std::env::set_var("HOME", home);
        std::env::remove_var("AINB_HOME");
        std::env::set_var("TMUX_TMPDIR", tmux_dir);
        std::env::remove_var("TMUX");
        std::env::remove_var(ainb_hangar_daemon::spawn::SPAWN_ENV);
    });
}

fn health() -> DaemonHealth {
    DaemonHealth {
        socket_path: "/tmp/mutation-dedupe.sock".to_string(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "0.1.0".into(),
        stats: std::sync::Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    }
}

fn request(method: &str, params: serde_json::Value) -> RpcRequest {
    RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(1),
        method: method.to_string(),
        params,
    }
}

/// The ack the daemon attached, from either half of the envelope.
///
/// A mutation that succeeded carries it beside its result; one that was refused
/// carries it in `error.data`. Both are the same contract ("what happened to
/// your op id"), so the assertions read the same either way.
fn ack(response: &serde_json::Value) -> serde_json::Value {
    if let Some(found) = response.get("result").and_then(|r| r.get(ACK_KEY)) {
        return found.clone();
    }
    response
        .get("error")
        .and_then(|e| e.get("data"))
        .and_then(|d| d.get(ACK_KEY))
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

/// Strip the ack so two replies can be compared for "is this the same answer".
fn without_ack(mut response: serde_json::Value) -> serde_json::Value {
    if let Some(result) = response.get_mut("result").and_then(|r| r.as_object_mut()) {
        result.remove(ACK_KEY);
    }
    if let Some(data) = response
        .get_mut("error")
        .and_then(|e| e.get_mut("data"))
        .and_then(|d| d.as_object_mut())
    {
        data.remove(ACK_KEY);
    }
    response
}

async fn dispatch(
    store: &Store,
    events: &ainb_hangar_daemon::events::EventSink,
    caller: &Caller,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let req = request(method, params);
    let response = tokio::time::timeout(
        PER_CALL,
        rpc::dispatch_as(store.pool(), &req, &health(), events, caller),
    )
    .await
    .unwrap_or_else(|_| panic!("{method} did not answer within {PER_CALL:?}"));
    serde_json::to_value(response).unwrap()
}

/// Criterion 1: replay every mutating method twice, one `created` and one
/// `replayed` each.
#[tokio::test]
async fn every_mutating_method_replays_exactly_once() {
    private_host();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let broker = EventBroker::new();
    let events = broker.sink();
    let mut deduplicated = 0usize;
    let mut transient: Vec<&str> = Vec::new();
    let mut refused: Vec<&str> = Vec::new();
    let mut unregistered: Vec<&str> = Vec::new();

    for (index, entry) in MUTATING_METHODS.iter().enumerate() {
        let mut params: serde_json::Value = serde_json::from_str(entry.sample_params)
            .unwrap_or_else(|e| panic!("{}: sample is not JSON: {e}", entry.method));
        let op_id = format!("op-dedupe-{index:03}");
        params
            .as_object_mut()
            .unwrap()
            .insert("op_id".to_string(), serde_json::json!(op_id));

        let first = dispatch(
            &store,
            &events,
            &Caller::Operator,
            entry.method,
            params.clone(),
        )
        .await;
        let second = dispatch(&store, &events, &Caller::Operator, entry.method, params).await;

        if refusal(&first).is_some() {
            // A handler that REFUSED did not mutate, so the ledger keeps no row
            // and the SAME op id stays free to deliver on a later attempt. Both
            // dispatches must therefore look identical and neither may claim a
            // replay: the opposite of the dedupe contract, and correct.
            assert_eq!(
                ack(&first)["status"],
                "rejected",
                "{} refused and must say so: {first}",
                entry.method
            );
            assert!(
                ack(&second)["outcome"] != "replayed",
                "{} refused, so nothing may be replayed: {second}",
                entry.method
            );
            assert_eq!(
                without_ack(second.clone()),
                without_ack(first.clone()),
                "{} did not answer its retry the same way",
                entry.method
            );
            refused.push(entry.method);
            continue;
        }

        if is_transient(&first) {
            // The OTHER half of the contract, asserted just as hard. A store
            // fault or an unavailable service means nothing ran, so the claim
            // is abandoned and the retry the caller is about to make is a real
            // retry: pinning a transient failure to an op id forever would be
            // the worse bug. Both dispatches must therefore look identical and
            // carry no ack at all.
            assert_eq!(
                ack(&first),
                serde_json::Value::Null,
                "{} answered transiently and must not have recorded an ack: {first}",
                entry.method
            );
            assert_eq!(
                without_ack(second.clone()),
                without_ack(first.clone()),
                "{} did not answer its retry the same way",
                entry.method
            );
            transient.push(entry.method);
            continue;
        }

        if is_unregistered(&first) {
            // A spawn verb refused before it ran anything: its sample path is
            // in no registered folder of this empty home. The op id stays
            // free so the same retry executes once the project is added.
            // Like a transient answer, it carries no ack and is not replayed.
            assert_eq!(
                ack(&first),
                serde_json::Value::Null,
                "{} was refused as unregistered and must not have recorded an ack: {first}",
                entry.method
            );
            assert_eq!(
                without_ack(second.clone()),
                without_ack(first.clone()),
                "{} did not answer its retry the same way",
                entry.method
            );
            unregistered.push(entry.method);
            continue;
        }

        assert_eq!(
            ack(&first)["outcome"],
            "created",
            "{} first dispatch must be created: {first}",
            entry.method
        );
        assert_eq!(
            ack(&second)["outcome"],
            "replayed",
            "{} second dispatch must be replayed: {second}",
            entry.method
        );
        assert_eq!(
            without_ack(second),
            without_ack(first),
            "{} replayed a DIFFERENT answer",
            entry.method
        );
        deduplicated += 1;
    }

    // The gate is the COVERAGE, not just the per-method assertion: a registry
    // that quietly shrank, or a daemon fixture in which everything answered
    // transiently, would otherwise pass with nothing proven. The floor is
    // len() - 1 because exactly ONE method answers transiently in this fixture
    // (`codex/session_ensure`, whose remote control is deliberately never
    // started). A second silent hole fails the gate.
    eprintln!(
        "deduplicated {deduplicated} of {} mutating methods; \
         transient: {transient:?}; refused: {refused:?}; unregistered: {unregistered:?}",
        MUTATING_METHODS.len()
    );
    // NAMED, not counted. A count leaves room for one hole to close while
    // another opens; naming the exceptions means a new one fails the gate and
    // has to be justified in the same change.
    assert_eq!(
        transient,
        vec!["codex/session_ensure"],
        "the only method allowed to answer transiently is the one whose remote \
         control this fixture deliberately never starts"
    );
    assert_eq!(
        refused,
        vec!["attention/answer"],
        "the only method allowed to refuse here is the one with no live session \
         to answer into; a refusal is not recorded, by design"
    );
    // No method is dark: the spawn verbs are served by default, so a
    // `METHOD_NOT_FOUND` here fails the `created` assertion above.
    assert_eq!(
        unregistered,
        vec!["shell/create", "worktree/create"],
        "the only methods allowed to refuse as unregistered are the spawn verbs \
         whose samples name a repository, in a home that registers none"
    );
    assert_eq!(
        deduplicated,
        MUTATING_METHODS.len() - transient.len() - refused.len() - unregistered.len(),
        "every other method must reach the ledger"
    );
}

/// Whether this response is a refusal the ledger deliberately did NOT record.
///
/// Keyed on the D18 reason vocabulary, not on `status: rejected` alone: a
/// RECORDED rejection (an `INVALID_PARAMS` stored as that op id's answer)
/// is also `rejected`, and it replays, which is the opposite contract.
fn refusal(response: &serde_json::Value) -> Option<String> {
    let reason = ack(response)["reason"].as_str()?.to_string();
    [
        ainb_hangar_proto::mutation::REASON_ALREADY_ANSWERED_BY,
        ainb_hangar_proto::mutation::REASON_NOT_DELIVERED,
        ainb_hangar_proto::mutation::REASON_NO_TARGET,
    ]
    .contains(&reason.as_str())
    .then_some(reason)
}

/// The two codes that mean "nothing happened, ask again": `SQLite` contention and
/// the daemon's internal catch-all (which carries "still starting" for a
/// service this test fixture deliberately never starts).
fn is_transient(response: &serde_json::Value) -> bool {
    let code = response["error"]["code"].as_i64();
    code == Some(i64::from(ainb_hangar_proto::STORE_UNAVAILABLE)) || code == Some(-32603)
}

/// `REPO_NOT_REGISTERED`: a spawn verb whose repository no registered folder
/// or project admits, refused before it ran anything.
fn is_unregistered(response: &serde_json::Value) -> bool {
    response["error"]["code"].as_i64()
        == Some(i64::from(ainb_hangar_proto::spawn::REPO_NOT_REGISTERED))
}

/// Criterion 1, second half (amendment 15): a second principal replaying the
/// same op id is refused, and nothing runs.
#[tokio::test]
async fn a_second_principal_replaying_an_op_id_is_rejected() {
    private_host();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let broker = EventBroker::new();
    let events = broker.sink();

    // `fleet/message_send`, and the choice is forced from both sides: the test
    // needs a method whose outcome is RECORDED (an answer with no live session
    // is a refusal, which by design leaves no row to collide with) AND one the
    // Pal credential is allowed to call at all. That is the intersection.
    //
    // Its op id rides on `request_id`, which IS the op id for that family
    // (amendment 19), so this also exercises the alias.
    let params = serde_json::json!({
        "targets": ["fleet-sample"],
        "text": "ship it",
        "request_id": "op-shared-across-principals",
    });

    let mine = dispatch(
        &store,
        &events,
        &Caller::Operator,
        ainb_hangar_proto::methods::FLEET_MESSAGE_SEND,
        params.clone(),
    )
    .await;
    assert_eq!(ack(&mine)["outcome"], "created", "{mine}");

    // Pal is a different credential and therefore a different principal.
    let theirs = dispatch(
        &store,
        &events,
        &Caller::Pal {
            scope_key: "channel:01JHARNESS".to_string(),
        },
        ainb_hangar_proto::methods::FLEET_MESSAGE_SEND,
        params,
    )
    .await;
    assert_eq!(
        theirs["error"]["code"],
        ainb_hangar_proto::mutation::MUTATION_REJECTED,
        "{theirs}"
    );
    assert_eq!(ack(&theirs)["status"], "rejected", "{theirs}");
    assert_eq!(ack(&theirs)["reason"], "op_id_foreign", "{theirs}");
}

/// Amendment 18: `adopted` requires the body to match. Two surfaces answering
/// the same question differently are not one operation, even under one op id.
#[tokio::test]
async fn the_same_op_id_with_a_different_body_is_rejected() {
    private_host();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let broker = EventBroker::new();
    let events = broker.sink();
    // Same reasoning as the foreign-principal test: a recorded outcome is the
    // precondition for a body-mismatch collision.
    let method = ainb_hangar_proto::methods::HANGAR_ISSUE_UPDATE;

    let yes = serde_json::json!({
        "workspace_id": "ws-sample",
        "issue_id": "iss-sample",
        "state": "todo",
        "op_id": "op-body-fingerprint",
    });
    let no = serde_json::json!({
        "workspace_id": "ws-sample",
        "issue_id": "iss-sample",
        "state": "doing",
        "op_id": "op-body-fingerprint",
    });

    let first = dispatch(&store, &events, &Caller::Operator, method, yes).await;
    assert_eq!(ack(&first)["outcome"], "created", "{first}");

    let clash = dispatch(&store, &events, &Caller::Operator, method, no).await;
    assert_eq!(
        clash["error"]["code"],
        ainb_hangar_proto::mutation::MUTATION_REJECTED,
        "{clash}"
    );
    assert_eq!(ack(&clash)["reason"], "already_answered_by", "{clash}");
}

/// A client that sends no op id is served exactly as it was before W0-wire.
/// This is the property that keeps the N-1 leg of the skew matrix green.
#[tokio::test]
async fn a_request_without_an_op_id_is_untouched() {
    private_host();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let broker = EventBroker::new();
    let events = broker.sink();

    let params = serde_json::json!({
        "attention_id": "att-no-op-id",
        "answer": "yes",
        "answered_by": "tui",
    });
    let first = dispatch(
        &store,
        &events,
        &Caller::Operator,
        ainb_hangar_proto::methods::ATTENTION_ANSWER,
        params.clone(),
    )
    .await;
    let second = dispatch(
        &store,
        &events,
        &Caller::Operator,
        ainb_hangar_proto::methods::ATTENTION_ANSWER,
        params,
    )
    .await;
    assert_eq!(ack(&first), serde_json::Value::Null, "{first}");
    assert_eq!(ack(&second), serde_json::Value::Null, "{second}");
    assert_eq!(
        first, second,
        "an un-deduplicated call must behave as before"
    );
}

/// #1066: a claim made before the daemon minted its host stays under `local`,
/// so a retry across the upgrade is still a replay; a claim made after the mint
/// is keyed under the minted host.
#[tokio::test]
async fn an_op_id_claimed_before_the_mint_replays_after_it() {
    private_host();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let broker = EventBroker::new();
    let events = broker.sink();
    let method = ainb_hangar_proto::methods::HANGAR_ISSUE_UPDATE;
    let body = |op_id: &str| {
        serde_json::json!({
            "workspace_id": "ws-sample",
            "issue_id": "iss-sample",
            "state": "todo",
            "op_id": op_id,
        })
    };
    let host_of = |op_id: &'static str| {
        let pool = store.pool().clone();
        async move {
            sqlx::query_scalar::<_, String>("SELECT host_id FROM mutation_ledger WHERE op_id = ?")
                .bind(op_id)
                .fetch_all(&pool)
                .await
                .unwrap()
        }
    };

    let before = dispatch(
        &store,
        &events,
        &Caller::Operator,
        method,
        body("op-upgrade"),
    )
    .await;
    assert_eq!(ack(&before)["outcome"], "created", "{before}");
    assert_eq!(host_of("op-upgrade").await, vec!["local"]);

    let host_id = ainb_hangar_store::repo::daemon_identity::DaemonIdentityRepo::mint_or_read(
        store.pool(),
        &ainb_hangar_core::idgen::SystemIdGen,
        &ainb_hangar_core::clock::SystemClock,
    )
    .await
    .unwrap()
    .identity
    .host_id;

    let retry = dispatch(
        &store,
        &events,
        &Caller::Operator,
        method,
        body("op-upgrade"),
    )
    .await;
    assert_eq!(ack(&retry)["outcome"], "replayed", "{retry}");
    assert_eq!(without_ack(retry), without_ack(before));
    assert_eq!(host_of("op-upgrade").await, vec!["local"], "no second row");

    let after = dispatch(
        &store,
        &events,
        &Caller::Operator,
        method,
        body("op-minted"),
    )
    .await;
    assert_eq!(ack(&after)["outcome"], "created", "{after}");
    assert_eq!(host_of("op-minted").await, vec![host_id]);
}

/// A dark verb's refusal frees its op id: `shell/create` with op id X answers
/// `METHOD_NOT_FOUND` from a daemon that does not serve it (started with
/// `AINB_HANGAR_SPAWN=0`, or from before the verb), and the same op id and
/// body runs for real once the verb is served. Recorded instead, the retry
/// after the flip would be replayed the old refusal and never open a shell.
///
/// Once it has run, the op id is taken: a second call after the flip is
/// `replayed` with the first answer and the handler does not run again.
///
/// The switch is read once per process and is on here, so both halves go
/// through the dispatcher's own ledger guard, the seam `dispatch` drives,
/// with a handler that refuses as a dark daemon does and then one that
/// answers.
#[tokio::test]
async fn a_dark_verb_leaves_its_op_id_free_for_the_flip() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    private_host();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let method = ainb_hangar_proto::methods::SHELL_CREATE;
    let body = serde_json::json!({ "worktree_path": "/tmp", "op_id": "op-dark-flip" });
    let now_ms = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();

    for attempt in ["first", "retry while dark"] {
        let refused = rpc::mutation::guard(
            store.pool(),
            &request(method, body.clone()),
            &Caller::Operator,
            now_ms,
            || async {
                Err(ainb_hangar_proto::RpcError {
                    code: -32601,
                    message: format!("unknown method: {method}"),
                    data: None,
                })
            },
        )
        .await
        .expect_err("a dark verb refuses");
        assert_eq!(refused.code, -32601, "{attempt}: {refused:?}");
        assert!(
            refused.data.as_ref().and_then(|d| d.get(ACK_KEY)).is_none(),
            "{attempt}: a dark refusal records no ack: {refused:?}"
        );
    }

    let runs = AtomicUsize::new(0);
    let served = serde_json::json!({
        "tmux_session_name": "ainb-dsh-0a1b2c3d",
        "worktree_path": "/tmp",
    });
    let serve = || async {
        runs.fetch_add(1, Ordering::SeqCst);
        Ok(served.clone())
    };

    let after = rpc::mutation::guard(
        store.pool(),
        &request(method, body.clone()),
        &Caller::Operator,
        now_ms,
        serve,
    )
    .await
    .expect("the served verb answers");
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "the retry after the flip must execute"
    );
    assert_eq!(after[ACK_KEY]["outcome"], "created", "{after}");
    assert_eq!(after["tmux_session_name"], "ainb-dsh-0a1b2c3d", "{after}");

    let again = rpc::mutation::guard(
        store.pool(),
        &request(method, body),
        &Caller::Operator,
        now_ms,
        serve,
    )
    .await
    .expect("the replay answers");
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "a second call with the same op id must not run the handler again"
    );
    assert_eq!(again[ACK_KEY]["outcome"], "replayed", "{again}");
    let answer = |mut value: serde_json::Value| {
        value.as_object_mut().expect("an object").remove(ACK_KEY);
        value
    };
    assert_eq!(answer(again), answer(after), "the first answer, replayed");
}
