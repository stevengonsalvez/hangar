//! The per-scope method table is a COMMITTED file, and every method is in it.
//!
//! A Contracts-job gate (C3, C4). Four properties:
//!
//! 1. **No default.** Every method in `ALL_METHODS` has exactly one row in
//!    `SCOPE_TABLE`, with an explicit verdict in every column. A new method
//!    that is not classified fails here, so it can never reach a remote
//!    laptop or a phone by omission.
//! 2. **The committed record.** `scopes.catalogue` is a rendering of the table.
//!    A changed verdict is a one-line diff a reviewer reads.
//! 3. **The lattice.** For every method and every params shape,
//!    allowed(mobile) implies allowed(mobile+type) implies allowed(desktop)
//!    implies allowed(desktop+admin) implies allowed(operator).
//! 4. **The security cases** S1 (admin is desktop only; no device mints
//!    devices), S5 (a read-only phone cannot take the floor), S7 (daemon config
//!    is operator only) and S10 (nothing reaches a device by default).
//!
//! Regenerate after an intentional change:
//! `UPDATE_SCOPE_CATALOGUE=1 cargo test -p ainb-hangar-proto --test scope_catalogue`,
//! then commit the diff.

use std::collections::HashSet;
use std::fmt::Write as _;

use ainb_hangar_proto::devices::{
    BaseScope, CallParams, DeviceScope, EVENT_TABLE, EventFamily, SCOPE_TABLE, ScopeColumn,
    Verdict, column_allows, column_receives, method_allowed,
};
use ainb_hangar_proto::fleet::ControlAction;
use ainb_hangar_proto::methods::{self as m, ALL_METHODS};
use ainb_hangar_proto::terminal::TerminalAttachParams;

const CATALOGUE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/scopes.catalogue");
const COMMITTED: &str = include_str!("../scopes.catalogue");

fn render() -> String {
    let width = SCOPE_TABLE
        .iter()
        .map(|r| r.method.len())
        .max()
        .unwrap_or(0)
        .max("event transcript".len());
    let mut out = String::new();
    out.push_str(
        "# The per-scope method and event table (D13, frozen by the v2-next PR-0).\n\
         #\n\
         # GENERATED from ainb_hangar_proto::devices::{SCOPE_TABLE, EVENT_TABLE}.\n\
         # Every method in ALL_METHODS has one row and an explicit verdict in every\n\
         # column: there is NO default. A changed line here is a changed grant.\n\
         #\n\
         # allow      always\n\
         # deny       never\n\
         # interrupt  fleet/action only as ControlAction::Interrupt (typed check)\n\
         # watch      terminal/attach only without want_input\n\
         #\n\
         # Regenerate: UPDATE_SCOPE_CATALOGUE=1 cargo test -p ainb-hangar-proto --test scope_catalogue\n\
         #\n",
    );
    let header: Vec<&str> = ScopeColumn::ALL.iter().map(|c| c.as_str()).collect();
    let _ = writeln!(
        out,
        "# {:<w$} {}",
        "method",
        header.join(" "),
        w = width - 2
    );
    for row in SCOPE_TABLE {
        out.push_str(&line(row.method, |c| row.verdict(c), width));
    }
    out.push_str("#\n");
    for row in EVENT_TABLE {
        let key = format!("event {}", row.family.as_str());
        out.push_str(&line(&key, |c| row.verdict(c), width));
    }
    out
}

fn line(key: &str, verdict: impl Fn(ScopeColumn) -> Verdict, width: usize) -> String {
    let cells: Vec<String> = ScopeColumn::ALL
        .iter()
        .map(|c| format!("{:<w$}", verdict(*c).as_str(), w = c.as_str().len()))
        .collect();
    format!("{key:<width$} {}\n", cells.join(" ").trim_end())
}

#[test]
fn the_table_matches_the_committed_catalogue() {
    let live = render();
    if std::env::var_os("UPDATE_SCOPE_CATALOGUE").is_some() {
        std::fs::write(CATALOGUE_PATH, &live).expect("write scopes.catalogue");
        return;
    }
    assert!(
        live == COMMITTED,
        "scopes.catalogue differs from SCOPE_TABLE / EVENT_TABLE. If the change is \
         intended, regenerate with UPDATE_SCOPE_CATALOGUE=1 and commit the diff.\n\
         --- live ---\n{live}"
    );
}

/// No default: every method is classified exactly once, and nothing else is.
#[test]
fn every_method_is_classified_exactly_once() {
    let mut seen = HashSet::new();
    for row in SCOPE_TABLE {
        assert!(seen.insert(row.method), "{} classified twice", row.method);
        assert!(
            ALL_METHODS.contains(&row.method),
            "{} is classified but is not a method",
            row.method
        );
    }
    let unclassified: Vec<&&str> = ALL_METHODS.iter().filter(|m| !seen.contains(*m)).collect();
    assert!(
        unclassified.is_empty(),
        "unclassified methods {unclassified:?}: add a row to SCOPE_TABLE with an \
         explicit verdict for every scope. There is no default."
    );
    let order: Vec<&str> = SCOPE_TABLE.iter().map(|r| r.method).collect();
    assert_eq!(order, ALL_METHODS.to_vec(), "rows follow ALL_METHODS order");
}

/// A params rule is meaningful only on the method whose params it reads.
#[test]
fn params_rules_sit_only_on_their_methods() {
    for row in SCOPE_TABLE {
        for column in ScopeColumn::ALL {
            match row.verdict(column) {
                Verdict::InterruptOnly => assert_eq!(row.method, m::FLEET_ACTION),
                Verdict::WatchOnly => assert_eq!(row.method, m::TERMINAL_ATTACH),
                Verdict::Allow | Verdict::Deny => {}
            }
        }
    }
    for row in EVENT_TABLE {
        for column in ScopeColumn::ALL {
            assert!(
                matches!(row.verdict(column), Verdict::Allow | Verdict::Deny),
                "event {:?} has a params rule",
                row.family
            );
        }
    }
}

fn attach(want_input: bool) -> TerminalAttachParams {
    serde_json::from_value(serde_json::json!({
        "session": {"host_id": "local", "session_key": "claude:a"},
        "want_input": want_input,
    }))
    .unwrap()
}

/// C4: the lattice, for every method and every params shape a rule can see.
#[test]
fn the_scope_lattice_holds_for_every_method_and_params() {
    let watch = attach(false);
    let typing = attach(true);
    let actions = [
        ControlAction::Interrupt,
        ControlAction::Kill,
        ControlAction::Stop,
        ControlAction::Archive,
        ControlAction::Continue,
        ControlAction::SendPrompt {
            text: "rm -rf".to_string(),
        },
    ];
    let mut shapes = vec![
        CallParams::Untyped,
        CallParams::TerminalAttach(&watch),
        CallParams::TerminalAttach(&typing),
    ];
    shapes.extend(actions.iter().map(CallParams::FleetAction));

    // Lowest first.
    let chain = [
        ScopeColumn::Mobile,
        ScopeColumn::MobileType,
        ScopeColumn::Desktop,
        ScopeColumn::DesktopAdmin,
        ScopeColumn::Operator,
    ];
    for method in ALL_METHODS {
        for params in &shapes {
            for pair in chain.windows(2) {
                if column_allows(pair[0], method, params) {
                    assert!(
                        column_allows(pair[1], method, params),
                        "{method} {params:?}: {} allows but {} refuses",
                        pair[0].as_str(),
                        pair[1].as_str()
                    );
                }
            }
        }
    }
    for family in EventFamily::ALL {
        for pair in chain.windows(2) {
            if column_receives(pair[0], family) {
                assert!(column_receives(pair[1], family), "{family:?}");
            }
        }
    }
}

/// The phone's grant is exactly the agreed list (R1-plan, M1 C-R1-7, R2 3.2).
#[test]
fn the_mobile_grant_is_exactly_the_agreed_list() {
    let allowed = |column| -> Vec<&str> {
        ALL_METHODS
            .iter()
            .copied()
            .filter(|method| {
                SCOPE_TABLE
                    .iter()
                    .find(|r| r.method == *method)
                    .is_some_and(|r| r.verdict(column) != Verdict::Deny)
            })
            .collect()
    };
    let mut mobile = allowed(ScopeColumn::Mobile);
    mobile.sort_unstable();
    let mut want = vec![
        m::AUTH_HELLO,
        m::PING,
        m::FLEET_NEGOTIATE,
        m::FLEET_SNAPSHOT,
        m::FLEET_STATUS,
        m::FLEET_ROSTER_STATUS,
        m::FLEET_SUBSCRIBE,
        m::ATTENTION_LIST,
        m::ATTENTION_SUBSCRIBE,
        m::ATTENTION_ANSWER,
        m::FLEET_MESSAGE_SEND,
        m::FLEET_ACTION,
        m::FLEET_TRANSCRIPT_LIST,
        m::FLEET_TRANSCRIPT_SUBSCRIBE,
        m::FLEET_RECEIPT_GET,
        m::TERMINAL_ATTACH,
        m::TERMINAL_DETACH,
        m::TERMINAL_ACK,
        m::TERMINAL_SCROLLBACK,
    ];
    want.sort_unstable();
    assert_eq!(mobile, want);

    let mut typing = allowed(ScopeColumn::MobileType);
    typing.sort_unstable();
    want.extend([m::TERMINAL_INPUT, m::TERMINAL_RESIZE, m::TERMINAL_FLOOR]);
    want.sort_unstable();
    assert_eq!(typing, want);
}

/// S1: admin is desktop only, and no device, admin or not, mints devices or
/// redeems again; admin alone gets list, revoke and rescope.
#[test]
fn s1_no_device_mints_devices() {
    assert!(DeviceScope::new(BaseScope::Mobile, true).is_err());
    assert!(DeviceScope::new(BaseScope::MobileType, true).is_err());
    for scope in [
        DeviceScope::MOBILE,
        DeviceScope::MOBILE_TYPE,
        DeviceScope::DESKTOP,
        DeviceScope::DESKTOP_ADMIN,
    ] {
        for method in [m::DEVICE_INVITE_CREATE, m::DEVICE_REDEEM] {
            assert!(
                !method_allowed(&scope, method, &CallParams::Untyped),
                "{scope:?} may call {method}"
            );
        }
        for method in [m::DEVICE_LIST, m::DEVICE_REVOKE, m::DEVICE_RESCOPE] {
            assert_eq!(
                method_allowed(&scope, method, &CallParams::Untyped),
                scope.admin(),
                "{scope:?} {method}"
            );
        }
    }
    assert!(column_allows(
        ScopeColumn::Operator,
        m::DEVICE_INVITE_CREATE,
        &CallParams::Untyped
    ));
    assert!(!column_allows(
        ScopeColumn::Operator,
        m::DEVICE_REDEEM,
        &CallParams::Untyped
    ));
}

/// S5: `want_input` needs mobile+type.
#[test]
fn s5_a_read_only_phone_cannot_take_the_floor() {
    let typing = attach(true);
    let watch = attach(false);
    assert!(!method_allowed(
        &DeviceScope::MOBILE,
        m::TERMINAL_ATTACH,
        &CallParams::TerminalAttach(&typing)
    ));
    assert!(method_allowed(
        &DeviceScope::MOBILE,
        m::TERMINAL_ATTACH,
        &CallParams::TerminalAttach(&watch)
    ));
    for method in [m::TERMINAL_INPUT, m::TERMINAL_FLOOR, m::TERMINAL_RESIZE] {
        assert!(!method_allowed(
            &DeviceScope::MOBILE,
            method,
            &CallParams::Untyped
        ));
        assert!(method_allowed(
            &DeviceScope::MOBILE_TYPE,
            method,
            &CallParams::Untyped
        ));
    }
}

/// C5 / S4: a phone may interrupt and nothing else, on the typed action.
#[test]
fn c5_a_phone_may_only_interrupt() {
    for scope in [DeviceScope::MOBILE, DeviceScope::MOBILE_TYPE] {
        assert!(method_allowed(
            &scope,
            m::FLEET_ACTION,
            &CallParams::FleetAction(&ControlAction::Interrupt)
        ));
        for action in [
            ControlAction::Kill,
            ControlAction::Stop,
            ControlAction::Retry,
        ] {
            assert!(!method_allowed(
                &scope,
                m::FLEET_ACTION,
                &CallParams::FleetAction(&action)
            ));
        }
        assert!(!method_allowed(
            &scope,
            m::FLEET_ACTION,
            &CallParams::Untyped
        ));
    }
}

/// S7 and the transcript wildcard: operator only.
#[test]
fn s7_host_wide_knobs_and_prune_are_operator_only() {
    for method in [m::HANGAR_DAEMON_CONFIG_SET, m::FLEET_TRANSCRIPT_PRUNE] {
        for scope in [
            DeviceScope::MOBILE,
            DeviceScope::MOBILE_TYPE,
            DeviceScope::DESKTOP,
            DeviceScope::DESKTOP_ADMIN,
        ] {
            assert!(
                !method_allowed(&scope, method, &CallParams::Untyped),
                "{method}"
            );
        }
        assert!(column_allows(
            ScopeColumn::Operator,
            method,
            &CallParams::Untyped
        ));
    }
}

/// The operator is unchanged by PR-0: it keeps every method it had, and the
/// only method it is refused is the peer-leg-only `device/redeem`.
#[test]
fn the_operator_keeps_every_method() {
    let refused: Vec<&str> = ALL_METHODS
        .iter()
        .copied()
        .filter(|method| !column_allows(ScopeColumn::Operator, method, &CallParams::Untyped))
        .collect();
    assert_eq!(refused, vec![m::DEVICE_REDEEM]);
}
