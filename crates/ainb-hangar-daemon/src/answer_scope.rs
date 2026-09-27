//! The kind-based scope gate inside `attention/answer` (hooks-and-answers,
//! review F2).
//!
//! `attention/answer` is allowed in every scope column of the frozen table,
//! including the read-only `mobile` column. That is right for a plain question,
//! and wrong for a tool approval: an approval can be an arbitrary Bash command,
//! and `fleet/action` Approve is interrupt-only for both phone columns. So an
//! answer that APPROVES something takes `fleet/action` Approve's verdict for the
//! caller's column:
//!
//! | row kind | resolves a live hook hold | verdict |
//! |---|---|---|
//! | `approval` | any | `fleet/action` Approve |
//! | `ask_user_question` | yes | `fleet/action` Approve |
//! | `ask_user_question` | no | `attention/answer` (the frozen row) |
//! | every other kind | n/a | `attention/answer` (the frozen row) |
//!
//! The scope table itself is not edited. A refusal is `MUTATION_REJECTED` with
//! reason [`ainb_hangar_proto::mutation::REASON_SCOPE`], and nothing is claimed.

use ainb_hangar_proto::devices::{CallParams, ScopeColumn, column_allows};
use ainb_hangar_proto::fleet::ControlAction;
use ainb_hangar_proto::methods;
use ainb_hangar_store::repo::attention::AttentionKind;

use crate::rpc::auth::Caller;

/// The scope column a caller answers from: the operator column for the
/// operator's own token, the device's own column for a paired device. `None`
/// for a device scope with an unknown base (refused), and for Pal, which has
/// no column: it is judged by [`caller_may_answer`].
#[must_use]
pub fn column_of(caller: &Caller) -> Option<ScopeColumn> {
    match caller {
        Caller::Operator => Some(ScopeColumn::Operator),
        Caller::Pal { .. } => None,
        Caller::Device { scope, .. } => scope.column(),
    }
}

/// Whether `caller` may answer a row of `kind`.
///
/// Pal is an LLM-driven caller. It may call `attention/answer` for plain
/// questions, but it can never call `fleet/action` at all, so it is refused
/// every approval-class answer: an approval can run an arbitrary command.
#[must_use]
pub fn caller_may_answer(caller: &Caller, kind: AttentionKind, resolves_hold: bool) -> bool {
    match caller {
        Caller::Pal { .. } => !is_approval_class(kind, resolves_hold),
        other => column_of(other).is_some_and(|column| answer_allowed(column, kind, resolves_hold)),
    }
}

/// Whether answering a row of `kind` approves something: an approval, or an
/// ask whose answer resolves a live hook hold.
#[must_use]
pub const fn is_approval_class(kind: AttentionKind, resolves_hold: bool) -> bool {
    match kind {
        AttentionKind::Approval => true,
        AttentionKind::AskUserQuestion => resolves_hold,
        _ => false,
    }
}

/// Whether `column` may answer a row of `kind`.
#[must_use]
pub fn answer_allowed(column: ScopeColumn, kind: AttentionKind, resolves_hold: bool) -> bool {
    if is_approval_class(kind, resolves_hold) {
        let approve = ControlAction::Approve {
            request_fingerprint: String::new(),
            request_identity: None,
        };
        column_allows(
            column,
            methods::FLEET_ACTION,
            &CallParams::FleetAction(&approve),
        )
    } else {
        column_allows(column, methods::ATTENTION_ANSWER, &CallParams::Untyped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLUMNS: [ScopeColumn; 5] = [
        ScopeColumn::Operator,
        ScopeColumn::DesktopAdmin,
        ScopeColumn::Desktop,
        ScopeColumn::MobileType,
        ScopeColumn::Mobile,
    ];

    const KINDS: [AttentionKind; 7] = [
        AttentionKind::AskUserQuestion,
        AttentionKind::Approval,
        AttentionKind::CodexRequestUser,
        AttentionKind::Error,
        AttentionKind::Waiting,
        AttentionKind::Escalation,
        AttentionKind::DeliveryUnconfirmed,
    ];

    /// Every attention kind, with and without a live hold, against every
    /// scope column. A kind or a column added later fails the exhaustive
    /// matches below until it is classified here.
    #[test]
    fn every_kind_against_every_scope_column() {
        for kind in KINDS {
            // Exhaustive: a new kind is a compile error here.
            let approval_class_without_hold = match kind {
                AttentionKind::Approval => true,
                AttentionKind::AskUserQuestion
                | AttentionKind::CodexRequestUser
                | AttentionKind::Error
                | AttentionKind::Waiting
                | AttentionKind::Escalation
                | AttentionKind::DeliveryUnconfirmed => false,
            };
            for resolves_hold in [false, true] {
                let approval_class = approval_class_without_hold
                    || (kind == AttentionKind::AskUserQuestion && resolves_hold);
                for column in COLUMNS {
                    let phone = match column {
                        ScopeColumn::Operator
                        | ScopeColumn::DesktopAdmin
                        | ScopeColumn::Desktop => false,
                        ScopeColumn::MobileType | ScopeColumn::Mobile => true,
                    };
                    let want = !(approval_class && phone);
                    assert_eq!(
                        answer_allowed(column, kind, resolves_hold),
                        want,
                        "{kind:?} hold={resolves_hold} {column:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn plain_answers_follow_the_frozen_table() {
        // ATTENTION_ANSWER is allowed in every column today; this gate must
        // not narrow it for anything that is not approval-class.
        for column in COLUMNS {
            assert!(column_allows(
                column,
                methods::ATTENTION_ANSWER,
                &CallParams::Untyped
            ));
        }
    }

    #[test]
    fn callers_answer_from_their_own_column() {
        use ainb_hangar_proto::devices::DeviceScope;
        assert_eq!(column_of(&Caller::Operator), Some(ScopeColumn::Operator));
        assert_eq!(
            column_of(&Caller::Pal {
                scope_key: "s".into()
            }),
            None
        );
        for (scope, column) in [
            (DeviceScope::MOBILE, ScopeColumn::Mobile),
            (DeviceScope::MOBILE_TYPE, ScopeColumn::MobileType),
            (DeviceScope::DESKTOP, ScopeColumn::Desktop),
        ] {
            let device = Caller::Device {
                device_id: "d1".into(),
                scope,
            };
            assert_eq!(column_of(&device), Some(column));
        }
    }

    /// Every caller kind against every attention kind, with and without a hold.
    #[test]
    fn every_caller_against_every_kind() {
        use ainb_hangar_proto::devices::DeviceScope;
        let pal = Caller::Pal {
            scope_key: "s".into(),
        };
        let phone = Caller::Device {
            device_id: "d1".into(),
            scope: DeviceScope::MOBILE,
        };
        let desktop = Caller::Device {
            device_id: "d2".into(),
            scope: DeviceScope::DESKTOP,
        };
        for kind in KINDS {
            for resolves_hold in [false, true] {
                let approval = is_approval_class(kind, resolves_hold);
                assert!(caller_may_answer(&Caller::Operator, kind, resolves_hold));
                assert!(caller_may_answer(&desktop, kind, resolves_hold));
                assert_eq!(
                    caller_may_answer(&pal, kind, resolves_hold),
                    !approval,
                    "Pal {kind:?} hold={resolves_hold}"
                );
                assert_eq!(
                    caller_may_answer(&phone, kind, resolves_hold),
                    !approval,
                    "phone {kind:?} hold={resolves_hold}"
                );
            }
        }
    }
}
