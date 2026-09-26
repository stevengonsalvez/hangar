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

/// The scope column a caller answers from.
///
/// Today every caller on the unix leg is the operator: `Caller` has no device
/// arm until R1-04 (#71) lands, and a Pal credential never reaches
/// `attention/answer` (its method list excludes it). When the device arm
/// arrives it maps to the device's own column here, and the gate below applies
/// with no other change.
#[must_use]
pub const fn column_of(caller: &Caller) -> ScopeColumn {
    match caller {
        Caller::Operator | Caller::Pal { .. } => ScopeColumn::Operator,
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
    fn every_current_caller_answers_from_the_operator_column() {
        assert_eq!(column_of(&Caller::Operator), ScopeColumn::Operator);
        assert_eq!(
            column_of(&Caller::Pal {
                scope_key: "s".into()
            }),
            ScopeColumn::Operator
        );
    }
}
