//! Paired devices and their scopes (spec D13, R1).
//!
//! Frozen by PR-0. Nothing here runs yet: the daemon constructs no device
//! caller, dispatches no `device/*` method, and advertises neither
//! [`crate::protocol::CAP_DEVICES`] nor [`crate::protocol::CAP_SCOPES`].
//!
//! ## The scope lattice
//!
//! ```text
//! operator ⊇ desktop+admin ⊇ desktop ⊇ mobile+type ⊇ mobile
//! (unix leg)  (base desktop only)
//! ```
//!
//! [`SCOPE_TABLE`] gives EVERY method in [`crate::methods::ALL_METHODS`] an
//! explicit [`Verdict`] in every column. There is no default: a method missing
//! from the table is refused for every device, and
//! `tests/scope_catalogue.rs` fails the build until it is classified. That
//! test also diffs the table against the committed `scopes.catalogue` and
//! proves the lattice above for every method and params shape.
//!
//! ## Identity comes from the credential
//!
//! A device's id and scope come from its token row, never from the request:
//! `HelloParams.device` is advisory, a mismatch closes
//! [`crate::peer_close::UNAUTHENTICATED`] (4401), and `answered_by` and
//! `fleet/message_send.actor` are stamped `device:<id>` by the daemon.
//!
//! ## Params rules are checked on TYPED params
//!
//! Two methods are allowed to a phone only for some params: `fleet/action`
//! only as [`ControlAction::Interrupt`], and `terminal/attach` only without
//! `want_input`. [`method_allowed`] takes a [`CallParams`] the dispatcher builds
//! AFTER the method's own params struct parsed, never a raw JSON value, so a
//! parser differential cannot smuggle `kill` past an interrupt-only rule.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::fleet::ControlAction;
use crate::hosts::HostId;
use crate::methods as m;
use crate::mutation::MutationEnvelope;
use crate::protocol::ProtocolRange;
use crate::terminal::TerminalAttachParams;

/// The prefix of a plaintext device token (`mdd_…`); only its SHA-256 is
/// stored.
pub const DEVICE_TOKEN_PREFIX: &str = "mdd_";
/// The longest invite a caller may ask for, in seconds.
pub const INVITE_TTL_MAX_S: u32 = 300;
/// A device token expires this many days after the device was last seen; each
/// accepted hello slides it. There is no refresh RPC.
pub const DEVICE_IDLE_EXPIRY_DAYS: u32 = 90;

/// The base of a device scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BaseScope {
    /// A paired laptop: everything except the operator-only methods.
    #[serde(rename = "desktop")]
    Desktop,
    /// A phone that reads, answers and interrupts, and watches terminals.
    #[serde(rename = "mobile")]
    Mobile,
    /// A phone that may also type into a terminal.
    #[serde(rename = "mobile+type")]
    MobileType,
    /// A base a newer daemon defined and this build does not know. It grants
    /// nothing: every method and event is refused, and it covers no scope.
    #[serde(rename = "unknown", other)]
    Unknown,
}

impl BaseScope {
    /// The wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Mobile => "mobile",
            Self::MobileType => "mobile+type",
            Self::Unknown => "unknown",
        }
    }
}

/// Why a scope is not representable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeError {
    /// `admin` on a base other than `desktop`.
    AdminNeedsDesktop(BaseScope),
}

impl fmt::Display for ScopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AdminNeedsDesktop(base) => {
                write!(f, "admin needs base desktop, not {}", base.as_str())
            }
        }
    }
}

impl std::error::Error for ScopeError {}

/// A device's scope: a base plus the additive admin flag.
///
/// `admin` implies `base == desktop`. The constructor and the decoder both
/// refuse any other combination, so a phone that is also an admin cannot be
/// represented, let alone granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawDeviceScope")]
pub struct DeviceScope {
    base: BaseScope,
    admin: bool,
}

#[derive(Deserialize)]
struct RawDeviceScope {
    base: BaseScope,
    #[serde(default)]
    admin: bool,
}

impl TryFrom<RawDeviceScope> for DeviceScope {
    type Error = ScopeError;

    fn try_from(raw: RawDeviceScope) -> Result<Self, Self::Error> {
        Self::new(raw.base, raw.admin)
    }
}

impl DeviceScope {
    /// `mobile`.
    pub const MOBILE: Self = Self {
        base: BaseScope::Mobile,
        admin: false,
    };
    /// `mobile+type`.
    pub const MOBILE_TYPE: Self = Self {
        base: BaseScope::MobileType,
        admin: false,
    };
    /// `desktop`.
    pub const DESKTOP: Self = Self {
        base: BaseScope::Desktop,
        admin: false,
    };
    /// `desktop` with `admin`.
    pub const DESKTOP_ADMIN: Self = Self {
        base: BaseScope::Desktop,
        admin: true,
    };

    /// A scope, refusing `admin` on any base but `desktop`.
    pub const fn new(base: BaseScope, admin: bool) -> Result<Self, ScopeError> {
        if admin && !matches!(base, BaseScope::Desktop) {
            return Err(ScopeError::AdminNeedsDesktop(base));
        }
        Ok(Self { base, admin })
    }

    /// The base.
    #[must_use]
    pub const fn base(self) -> BaseScope {
        self.base
    }

    /// Whether the admin extras are granted.
    #[must_use]
    pub const fn admin(self) -> bool {
        self.admin
    }

    /// The [`SCOPE_TABLE`] column this scope reads, or `None` for a base this
    /// build does not know, which reads no column and is refused everything.
    #[must_use]
    pub const fn column(self) -> Option<ScopeColumn> {
        match (self.base, self.admin) {
            (BaseScope::Desktop, true) => Some(ScopeColumn::DesktopAdmin),
            (BaseScope::Desktop, false) => Some(ScopeColumn::Desktop),
            (BaseScope::MobileType, _) => Some(ScopeColumn::MobileType),
            (BaseScope::Mobile, _) => Some(ScopeColumn::Mobile),
            (BaseScope::Unknown, _) => None,
        }
    }

    /// Whether this scope is at least `other` in the lattice. An unknown base
    /// covers nothing and is covered by nothing.
    ///
    /// Ordering only: whether a caller may GRANT a scope is
    /// [`Grantor::may_grant`], which is stricter.
    #[must_use]
    pub const fn covers(self, other: Self) -> bool {
        match (self.column(), other.column()) {
            (Some(mine), Some(theirs)) => mine.rank() >= theirs.rank(),
            _ => false,
        }
    }
}

/// Who is granting a scope, through an invite or a rescope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grantor {
    /// The local operator on the unix leg (the CLI).
    Operator,
    /// A paired device, by its own scope.
    Device(DeviceScope),
}

impl Grantor {
    /// Whether this grantor may give a device `target`.
    ///
    /// DV5 and S1: only the operator grants `desktop` or `admin`. A device may
    /// grant only `mobile` or `mobile+type`, and never above its own scope, so
    /// an admin desktop can move a phone between the two phone scopes and no
    /// further. Nobody grants an unknown base.
    #[must_use]
    pub const fn may_grant(self, target: DeviceScope) -> bool {
        if target.column().is_none() {
            return false;
        }
        match self {
            Self::Operator => true,
            Self::Device(own) => {
                matches!(target.base, BaseScope::Mobile | BaseScope::MobileType)
                    && !target.admin
                    && own.covers(target)
            }
        }
    }
}

/// One column of [`SCOPE_TABLE`], lowest first in [`ScopeColumn::rank`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScopeColumn {
    /// The local operator on the unix leg; never a device.
    Operator,
    /// `desktop` with `admin`.
    DesktopAdmin,
    /// `desktop`.
    Desktop,
    /// `mobile+type`.
    MobileType,
    /// `mobile`.
    Mobile,
}

impl ScopeColumn {
    /// Every column, widest first, in catalogue order.
    pub const ALL: [Self; 5] = [
        Self::Operator,
        Self::DesktopAdmin,
        Self::Desktop,
        Self::MobileType,
        Self::Mobile,
    ];

    /// The catalogue header token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Operator => "operator",
            Self::DesktopAdmin => "desktop+admin",
            Self::Desktop => "desktop",
            Self::MobileType => "mobile+type",
            Self::Mobile => "mobile",
        }
    }

    /// Position in the lattice: a higher rank covers every lower one.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Mobile => 0,
            Self::MobileType => 1,
            Self::Desktop => 2,
            Self::DesktopAdmin => 3,
            Self::Operator => 4,
        }
    }
}

/// One method's verdict in one column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Verdict {
    /// Always allowed.
    Allow,
    /// Always refused.
    Deny,
    /// `fleet/action` only as [`ControlAction::Interrupt`].
    InterruptOnly,
    /// `terminal/attach` only without `want_input`.
    WatchOnly,
}

impl Verdict {
    /// The catalogue token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::InterruptOnly => "interrupt",
            Self::WatchOnly => "watch",
        }
    }

    /// Whether a call with `params` passes this verdict.
    ///
    /// A params rule with [`CallParams::Untyped`] refuses: the dispatcher must
    /// parse the method's own params before it can earn the narrower grant.
    #[must_use]
    pub const fn allows(self, params: &CallParams<'_>) -> bool {
        match self {
            Self::Allow => true,
            Self::Deny => false,
            Self::InterruptOnly => {
                matches!(params, CallParams::FleetAction(ControlAction::Interrupt))
            }
            Self::WatchOnly => {
                matches!(params, CallParams::TerminalAttach(p) if !p.want_input)
            }
        }
    }
}

/// The typed params a scope check may look at.
#[derive(Debug, Clone, Copy)]
pub enum CallParams<'a> {
    /// No params rule applies, or the params were not parsed.
    Untyped,
    /// A parsed `fleet/action` action.
    FleetAction(&'a ControlAction),
    /// Parsed `terminal/attach` params.
    TerminalAttach(&'a TerminalAttachParams),
}

/// One method's verdict in every column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopeRow {
    /// The method (a [`crate::methods`] const).
    pub method: &'static str,
    /// The local operator, on the unix leg.
    pub operator: Verdict,
    /// `desktop` with `admin`.
    pub desktop_admin: Verdict,
    /// `desktop`.
    pub desktop: Verdict,
    /// `mobile+type`.
    pub mobile_type: Verdict,
    /// `mobile`.
    pub mobile: Verdict,
}

impl ScopeRow {
    /// The verdict in `column`.
    #[must_use]
    pub const fn verdict(&self, column: ScopeColumn) -> Verdict {
        match column {
            ScopeColumn::Operator => self.operator,
            ScopeColumn::DesktopAdmin => self.desktop_admin,
            ScopeColumn::Desktop => self.desktop,
            ScopeColumn::MobileType => self.mobile_type,
            ScopeColumn::Mobile => self.mobile,
        }
    }
}

const fn row(
    method: &'static str,
    operator: Verdict,
    desktop_admin: Verdict,
    desktop: Verdict,
    mobile_type: Verdict,
    mobile: Verdict,
) -> ScopeRow {
    ScopeRow {
        method,
        operator,
        desktop_admin,
        desktop,
        mobile_type,
        mobile,
    }
}

const A: Verdict = Verdict::Allow;
const D: Verdict = Verdict::Deny;
const I: Verdict = Verdict::InterruptOnly;
const W: Verdict = Verdict::WatchOnly;

/// Every method, classified in every column. NO default.
///
/// Columns: operator, desktop+admin, desktop, mobile+type, mobile.
/// `A` allow, `D` deny, `I` interrupt only, `W` watch only (no `want_input`).
///
/// In [`crate::methods::ALL_METHODS`] order; a new method is appended here in
/// the change that declares it, and to `scopes.catalogue` beside it.
///
/// Deltas from the spec's D13 table, all recorded in the v2-next contract:
/// desktop loses `hangar/daemon_config_set` (operator only), admin does not
/// get `device/invite_create` (operator only in v1), plain desktop does not get
/// `hangar/connections_list` (its events are admin only), `fleet/transcript_prune`
/// is refused to every device, and `device/redeem` is refused to everyone
/// here because it is served before hello on the peer leg only.
pub static SCOPE_TABLE: &[ScopeRow] = &[
    row(m::WORKSPACE_SUBSCRIBE, A, A, A, D, D),
    row(m::WORKSPACE_LIST, A, A, A, D, D),
    row(m::WORKSPACE_SESSION_LIST, A, A, A, D, D),
    row(m::WORKSPACE_SESSION_UPSERT, A, A, A, D, D),
    row(m::WORKSPACE_SESSION_DELETE, A, A, A, D, D),
    row(m::WORKSPACE_SESSION_RECONCILE, A, A, A, D, D),
    row(m::HANGAR_ISSUES_LIST, A, A, A, D, D),
    row(m::HANGAR_ISSUES_SEARCH, A, A, A, D, D),
    row(m::HANGAR_SEARCH, A, A, A, D, D),
    row(m::HANGAR_AGENTS_LIST, A, A, A, D, D),
    row(m::HANGAR_SKILLS_LIST, A, A, A, D, D),
    row(m::HANGAR_SKILL_GET, A, A, A, D, D),
    row(m::HANGAR_SKILLS_SYNC, A, A, A, D, D),
    row(m::HANGAR_SKILL_ATTACH, A, A, A, D, D),
    row(m::HANGAR_SKILL_DETACH, A, A, A, D, D),
    row(m::HANGAR_SKILL_SET_ENABLED, A, A, A, D, D),
    row(m::HANGAR_AGENT_SKILLS_LIST, A, A, A, D, D),
    row(m::HANGAR_AUTOPILOTS_LIST, A, A, A, D, D),
    row(m::HANGAR_AUTOPILOT_RUNS, A, A, A, D, D),
    row(m::HANGAR_AUTOPILOT_FIRE_NOW, A, A, A, D, D),
    row(m::HANGAR_AUTOPILOT_SET_ENABLED, A, A, A, D, D),
    row(m::HANGAR_AUTOPILOT_TRIGGER_API, A, A, A, D, D),
    row(m::HANGAR_AUTOPILOT_SET_API_TRIGGER, A, A, A, D, D),
    row(m::HANGAR_AUTOPILOT_UPDATE, A, A, A, D, D),
    row(m::HANGAR_AUTOPILOT_VERSIONS, A, A, A, D, D),
    row(m::HANGAR_AUTOPILOT_COLLABORATOR_ADD, A, A, A, D, D),
    row(m::HANGAR_AUTOPILOT_COLLABORATOR_REMOVE, A, A, A, D, D),
    row(m::HANGAR_AUTOPILOT_COLLABORATORS, A, A, A, D, D),
    row(m::HANGAR_AUTOPILOT_SUBSCRIBER_ADD, A, A, A, D, D),
    row(m::HANGAR_AUTOPILOT_SUBSCRIBER_REMOVE, A, A, A, D, D),
    row(m::HANGAR_AUTOPILOT_SUBSCRIBERS, A, A, A, D, D),
    row(m::HANGAR_AUTOPILOT_SET_ACCESS_MODE, A, A, A, D, D),
    row(m::HANGAR_TASKS_LIST, A, A, A, D, D),
    row(m::HANGAR_TASK_TRANSITION, A, A, A, D, D),
    row(m::HANGAR_TASK_RETRY, A, A, A, D, D),
    row(m::HANGAR_ISSUE_UPDATE, A, A, A, D, D),
    row(m::HANGAR_ISSUES_BATCH_UPDATE, A, A, A, D, D),
    row(m::HANGAR_ISSUE_LABEL_ATTACH, A, A, A, D, D),
    row(m::HANGAR_ISSUE_LABEL_DETACH, A, A, A, D, D),
    row(m::HANGAR_ISSUE_CRITERION_SET, A, A, A, D, D),
    row(m::HANGAR_COMMENT_ADD, A, A, A, D, D),
    row(m::HANGAR_COMMENT_MENTION_PREVIEW, A, A, A, D, D),
    row(m::HANGAR_AGENT_UPDATE, A, A, A, D, D),
    row(m::HANGAR_AGENT_ARCHIVE, A, A, A, D, D),
    row(m::HANGAR_MEMBERS_LIST, A, A, A, D, D),
    row(m::HANGAR_MEMBER_SET_ROLE, A, A, A, D, D),
    row(m::HANGAR_MEMBER_REMOVE, A, A, A, D, D),
    row(m::HANGAR_INVITE_CREATE, A, A, A, D, D),
    row(m::HANGAR_INVITE_ACCEPT, A, A, A, D, D),
    row(m::HANGAR_INVITE_DECLINE, A, A, A, D, D),
    row(m::HANGAR_INVITE_REVOKE, A, A, A, D, D),
    row(m::HANGAR_SQUADS_LIST, A, A, A, D, D),
    row(m::HANGAR_SQUAD_CREATE, A, A, A, D, D),
    row(m::HANGAR_SQUAD_MEMBER_ADD, A, A, A, D, D),
    row(m::HANGAR_SQUAD_MEMBER_REMOVE, A, A, A, D, D),
    row(m::HANGAR_SQUAD_ASSIGN, A, A, A, D, D),
    row(m::HANGAR_SQUAD_ARCHIVE, A, A, A, D, D),
    row(m::HANGAR_SQUAD_MEMBER_ROLE_SET, A, A, A, D, D),
    row(m::HANGAR_SQUAD_INSTRUCTIONS_SET, A, A, A, D, D),
    row(m::HANGAR_HEALTH, A, A, A, D, D),
    row(m::HANGAR_DAEMON_HEALTH, A, A, A, D, D),
    row(m::HANGAR_USAGE_ROLLUP, A, A, A, D, D),
    row(m::HANGAR_PR_STATUS_REFRESH, A, A, A, D, D),
    row(m::HANGAR_INBOX_LIST, A, A, A, D, D),
    row(m::HANGAR_INBOX_MARK_READ, A, A, A, D, D),
    row(m::ATTENTION_LIST, A, A, A, A, A),
    row(m::ATTENTION_SUBSCRIBE, A, A, A, A, A),
    row(m::ATTENTION_ANSWER, A, A, A, A, A),
    row(m::ATC_REGISTER, A, A, A, D, D),
    row(m::ATC_LIST, A, A, A, D, D),
    row(m::ATC_ESCALATE, A, A, A, D, D),
    row(m::ATC_UNREGISTER, A, A, A, D, D),
    row(m::AUTH_HELLO, A, A, A, A, A),
    row(m::PING, A, A, A, A, A),
    row(m::HANGAR_BOARDS_LIST, A, A, A, D, D),
    row(m::HANGAR_BOARD_CREATE, A, A, A, D, D),
    row(m::HANGAR_BOARD_UPDATE, A, A, A, D, D),
    row(m::HANGAR_BOARD_DELETE, A, A, A, D, D),
    row(m::HANGAR_BOARD_COLUMN_ADD, A, A, A, D, D),
    row(m::HANGAR_BOARD_COLUMN_UPDATE, A, A, A, D, D),
    row(m::HANGAR_BOARD_COLUMN_DELETE, A, A, A, D, D),
    row(m::HANGAR_BOARD_COLUMN_REORDER, A, A, A, D, D),
    row(m::HANGAR_BOARD_CARD_ADD, A, A, A, D, D),
    row(m::HANGAR_BOARD_CARD_MOVE, A, A, A, D, D),
    row(m::HANGAR_SQUAD_FANOUT, A, A, A, D, D),
    row(m::HANGAR_RUN_HISTORY, A, A, A, D, D),
    row(m::PROFILE_LIST, A, A, A, D, D),
    row(m::PROFILE_GET, A, A, A, D, D),
    row(m::PROFILE_UPSERT, A, A, A, D, D),
    row(m::HANGAR_BOARD_CARD_CREATE, A, A, A, D, D),
    row(m::HANGAR_BOARD_CARD_RUN, A, A, A, D, D),
    row(m::HANGAR_REPO_LIST, A, A, A, D, D),
    row(m::HANGAR_BOARD_CARD_CANCEL, A, A, A, D, D),
    row(m::HANGAR_BOARD_CARD_REORDER, A, A, A, D, D),
    row(m::HANGAR_BOARD_CARD_REMOVE, A, A, A, D, D),
    row(m::HANGAR_BOARD_CARD_TIMELINE, A, A, A, D, D),
    row(m::HANGAR_BOARD_CARD_ASSIGN_SQUAD, A, A, A, D, D),
    row(m::HANGAR_BOARD_CARD_DEP_ADD, A, A, A, D, D),
    row(m::HANGAR_BOARD_CARD_DEP_REMOVE, A, A, A, D, D),
    row(m::HANGAR_BOARD_CARD_SET_AUTO_RUN, A, A, A, D, D),
    row(m::HANGAR_NOTIFY_RULES_LIST, A, A, A, D, D),
    row(m::HANGAR_NOTIFY_RULE_SET, A, A, A, D, D),
    row(m::HANGAR_DAEMON_CONFIG_GET, A, A, A, D, D),
    row(m::HANGAR_DAEMON_CONFIG_SET, A, D, D, D, D),
    row(m::HANGAR_AGENT_CREATE, A, A, A, D, D),
    row(m::HANGAR_DAEMON_CONFIG_LIST, A, A, A, D, D),
    row(m::HANGAR_CONNECTIONS_LIST, A, A, D, D, D),
    row(m::HANGAR_ISSUE_DELETE, A, A, A, D, D),
    row(m::HANGAR_ISSUE_CANCEL_ACTIVE, A, A, A, D, D),
    row(m::HANGAR_AGENT_DELETE, A, A, A, D, D),
    row(m::HANGAR_ISSUE_LINK_ADD, A, A, A, D, D),
    row(m::HANGAR_ISSUE_LINK_REMOVE, A, A, A, D, D),
    row(m::HANGAR_ISSUE_LINKS, A, A, A, D, D),
    row(m::HANGAR_ISSUE_SUBSCRIBE, A, A, A, D, D),
    row(m::HANGAR_ISSUE_UNSUBSCRIBE, A, A, A, D, D),
    row(m::HANGAR_ISSUE_SUBSCRIBERS, A, A, A, D, D),
    row(m::HANGAR_ISSUE_REACTION_ADD, A, A, A, D, D),
    row(m::HANGAR_ISSUE_REACTION_REMOVE, A, A, A, D, D),
    row(m::FLEET_SNAPSHOT, A, A, A, A, A),
    row(m::FLEET_STATUS, A, A, A, A, A),
    row(m::FLEET_ROSTER_STATUS, A, A, A, A, A),
    row(m::FLEET_SUBSCRIBE, A, A, A, A, A),
    row(m::FLEET_ACTION, A, A, A, I, I),
    row(m::FLEET_BROADCAST, A, A, A, D, D),
    row(m::FLEET_NEGOTIATE, A, A, A, A, A),
    row(m::FLEET_RECEIPT_LIST, A, A, A, D, D),
    row(m::FLEET_RECEIPT_GET, A, A, A, A, A),
    row(m::FLEET_START, A, A, A, D, D),
    row(m::CODEX_SESSION_ENSURE, A, A, A, D, D),
    row(m::FLEET_TIMELINE, A, A, A, D, D),
    row(m::FLEET_USAGE_SUMMARY, A, A, A, D, D),
    row(m::FLEET_USAGE_DASHBOARD, A, A, A, D, D),
    row(m::FLEET_QUOTA_SUMMARY, A, A, A, D, D),
    row(m::FLEET_RUNTIME_STATUS, A, A, A, D, D),
    row(m::FLEET_REPROJECT_CLAUDE_INTERVIEW, A, A, A, D, D),
    row(m::HANGAR_DISPATCH_ATTEMPTS_LIST, A, A, A, D, D),
    row(m::HANGAR_ISSUE_TIMELINE, A, A, A, D, D),
    row(m::HANGAR_PROPERTIES_LIST, A, A, A, D, D),
    row(m::HANGAR_PROPERTY_DEFINE, A, A, A, D, D),
    row(m::HANGAR_PROPERTY_ARCHIVE, A, A, A, D, D),
    row(m::HANGAR_ISSUE_PROPERTY_SET, A, A, A, D, D),
    row(m::HANGAR_ISSUE_PROPERTY_CLEAR, A, A, A, D, D),
    row(m::HANGAR_ISSUE_METADATA_GET, A, A, A, D, D),
    row(m::HANGAR_ISSUE_METADATA_SET, A, A, A, D, D),
    row(m::HANGAR_ISSUE_METADATA_DELETE, A, A, A, D, D),
    row(m::FLEET_ACP_SESSION_CREATE, A, A, A, D, D),
    row(m::FLEET_MESSAGE_SEND, A, A, A, A, A),
    row(m::FLEET_MESSAGE_LIST, A, A, A, D, D),
    row(m::FLEET_MESSAGE_SUBSCRIBE, A, A, A, D, D),
    row(m::FLEET_TRANSCRIPT_LIST, A, A, A, A, A),
    row(m::FLEET_TRANSCRIPT_SUBSCRIBE, A, A, A, A, A),
    row(m::FLEET_TRANSCRIPT_PRUNE, A, D, D, D, D),
    row(m::FLEET_CHANNEL_CREATE, A, A, A, D, D),
    row(m::FLEET_CHANNEL_LIST, A, A, A, D, D),
    row(m::FLEET_PAL_CONFIGURE, A, A, A, D, D),
    row(m::FLEET_CONFIRM_LIST, A, A, A, D, D),
    row(m::FLEET_CONFIRM_ANSWER, A, A, A, D, D),
    row(m::FLEET_ACTIVITY_LIST, A, A, A, D, D),
    row(m::FLEET_PAL_GATE, A, A, A, D, D),
    row(m::CODEX_SESSION_DISCARD, A, A, A, D, D),
    row(m::FLEET_ADAPTER_LIST, A, A, A, D, D),
    row(m::ATC_RETRY_LIST, A, A, A, D, D),
    row(m::DEVICE_REDEEM, D, D, D, D, D),
    row(m::DEVICE_INVITE_CREATE, A, D, D, D, D),
    row(m::DEVICE_LIST, A, A, D, D, D),
    row(m::DEVICE_REVOKE, A, A, D, D, D),
    row(m::DEVICE_RESCOPE, A, A, D, D, D),
    row(m::TERMINAL_ATTACH, A, A, A, A, W),
    row(m::TERMINAL_DETACH, A, A, A, A, A),
    row(m::TERMINAL_ACK, A, A, A, A, A),
    row(m::TERMINAL_SCROLLBACK, A, A, A, A, A),
    row(m::TERMINAL_INPUT, A, A, A, A, D),
    row(m::TERMINAL_FLOOR, A, A, A, A, D),
    row(m::TERMINAL_RESIZE, A, A, A, A, D),
    row(m::HANGAR_ISSUE_CREATE, A, A, A, D, D),
    row(m::HANGAR_ISSUE_RUN, A, A, A, D, D),
];

/// The row for `method`, when it is classified.
#[must_use]
pub fn scope_row(method: &str) -> Option<&'static ScopeRow> {
    SCOPE_TABLE.iter().find(|r| r.method == method)
}

/// Whether `column` may call `method` with `params`. An unclassified method is
/// refused.
#[must_use]
pub fn column_allows(column: ScopeColumn, method: &str, params: &CallParams<'_>) -> bool {
    scope_row(method).is_some_and(|r| r.verdict(column).allows(params))
}

/// Whether a device with `scope` may call `method` with `params`. An unknown
/// base is refused every method.
#[must_use]
pub fn method_allowed(scope: &DeviceScope, method: &str, params: &CallParams<'_>) -> bool {
    scope.column().is_some_and(|column| column_allows(column, method, params))
}

/// The event families a subscription stream carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventFamily {
    /// Workspace rows: issues, boards, agents, sessions.
    Workspace,
    /// Task stream output.
    TaskStream,
    /// The attention inbox.
    Attention,
    /// The live surface registry (`ConnectionsChanged`).
    Connections,
    /// Fleet session state.
    Fleet,
    /// Fleet messages.
    Message,
    /// Fleet transcripts.
    Transcript,
    /// Notification routing.
    Notification,
    /// The device registry.
    Devices,
    /// A family a newer daemon defined and this build does not know. No scope
    /// receives it.
    #[serde(other)]
    Unknown,
}

impl EventFamily {
    /// Every family.
    pub const ALL: [Self; 9] = [
        Self::Workspace,
        Self::TaskStream,
        Self::Attention,
        Self::Connections,
        Self::Fleet,
        Self::Message,
        Self::Transcript,
        Self::Notification,
        Self::Devices,
    ];

    /// The wire and catalogue token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::TaskStream => "task_stream",
            Self::Attention => "attention",
            Self::Connections => "connections",
            Self::Fleet => "fleet",
            Self::Message => "message",
            Self::Transcript => "transcript",
            Self::Notification => "notification",
            Self::Devices => "devices",
            Self::Unknown => "unknown",
        }
    }
}

/// One event family's verdict in every column; only `Allow` and `Deny` apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventRow {
    /// The family.
    pub family: EventFamily,
    /// The local operator.
    pub operator: Verdict,
    /// `desktop` with `admin`.
    pub desktop_admin: Verdict,
    /// `desktop`.
    pub desktop: Verdict,
    /// `mobile+type`.
    pub mobile_type: Verdict,
    /// `mobile`.
    pub mobile: Verdict,
}

impl EventRow {
    /// The verdict in `column`.
    #[must_use]
    pub const fn verdict(&self, column: ScopeColumn) -> Verdict {
        match column {
            ScopeColumn::Operator => self.operator,
            ScopeColumn::DesktopAdmin => self.desktop_admin,
            ScopeColumn::Desktop => self.desktop,
            ScopeColumn::MobileType => self.mobile_type,
            ScopeColumn::Mobile => self.mobile,
        }
    }
}

const fn event(
    family: EventFamily,
    operator: Verdict,
    desktop_admin: Verdict,
    desktop: Verdict,
    mobile_type: Verdict,
    mobile: Verdict,
) -> EventRow {
    EventRow {
        family,
        operator,
        desktop_admin,
        desktop,
        mobile_type,
        mobile,
    }
}

/// Every event family, classified in every column. NO default.
///
/// A phone never receives an event whose read method it cannot call.
/// `terminal/frame` is not here: it reaches only the connection that attached.
pub static EVENT_TABLE: &[EventRow] = &[
    event(EventFamily::Workspace, A, A, A, D, D),
    event(EventFamily::TaskStream, A, A, A, D, D),
    event(EventFamily::Attention, A, A, A, A, A),
    event(EventFamily::Connections, A, A, D, D, D),
    event(EventFamily::Fleet, A, A, A, A, A),
    event(EventFamily::Message, A, A, A, D, D),
    event(EventFamily::Transcript, A, A, A, A, A),
    event(EventFamily::Notification, A, A, A, D, D),
    event(EventFamily::Devices, A, A, D, D, D),
];

/// Whether `column` receives `family`.
#[must_use]
pub fn column_receives(column: ScopeColumn, family: EventFamily) -> bool {
    EVENT_TABLE
        .iter()
        .find(|r| r.family == family)
        .is_some_and(|r| matches!(r.verdict(column), Verdict::Allow))
}

/// Whether a device with `scope` receives `family`. An unknown base receives
/// nothing.
#[must_use]
pub fn event_allowed(scope: &DeviceScope, family: EventFamily) -> bool {
    scope.column().is_some_and(|column| column_receives(column, family))
}

/// One paired device, as `device/list` returns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRow {
    /// The device id, a ULID minted at redeem.
    pub device_id: String,
    /// The name given at pairing.
    pub display_name: String,
    /// The granted scope.
    pub scope: DeviceScope,
    /// Unix milliseconds of the redeem.
    pub created_at_ms: i64,
    /// Unix milliseconds the token expires, sliding on every hello.
    pub expires_at_ms: i64,
    /// Unix milliseconds of the last accepted hello.
    pub last_seen_at_ms: i64,
    /// Unix milliseconds of the revoke, when revoked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at_ms: Option<i64>,
    /// The SHA-256 hex digest of the device's Noise static public key.
    pub static_pubkey_digest: String,
}

/// `device/redeem` params: the first Rpc on the peer leg, in place of hello.
///
/// `Debug` redacts [`Self::invite_secret`].
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRedeemParams {
    /// The invite, a ULID.
    pub invite_id: String,
    /// The invite secret, base64url without padding (32 bytes).
    pub invite_secret: String,
    /// The name to list the device under.
    pub display_name: String,
    /// The protocol versions the device speaks.
    pub protocol: ProtocolRange,
}

/// `device/redeem` result. The next frame must be `auth/hello` with the token.
///
/// `Debug` redacts [`Self::device_token`].
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRedeemResult {
    /// The new device id.
    pub device_id: String,
    /// The plaintext token (`mdd_…`), shown once; bound to the Noise remote
    /// static key of this session.
    pub device_token: String,
    /// The granted scope.
    pub scope: DeviceScope,
    /// Unix milliseconds the token expires unless a hello slides it.
    pub expires_at_ms: i64,
    /// The host that paired the device, always minted: `local` is refused on
    /// decode.
    #[serde(deserialize_with = "crate::hosts::deserialize_minted")]
    pub host_id: HostId,
}

/// `device/invite_create` params (operator only in v1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInviteCreateParams {
    /// The scope the redeemed device gets. Never above the caller's own.
    pub scope: DeviceScope,
    /// A suggested name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Lifetime in seconds, at most [`INVITE_TTL_MAX_S`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_s: Option<u32>,
    /// The D18 envelope, flattened: `op_id?`, `fence?` at top level.
    #[serde(flatten)]
    pub mutation: MutationEnvelope,
}

/// `device/invite_create` result.
///
/// `Debug` redacts [`Self::offer`]: the URI carries the invite secret.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInviteCreateResult {
    /// The pairing URI, `ainb://pair#…`; also the QR payload.
    pub offer: String,
    /// The invite id.
    pub invite_id: String,
    /// Unix milliseconds the invite expires.
    pub expires_at_ms: i64,
}

impl fmt::Debug for DeviceRedeemParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            invite_id,
            invite_secret: _,
            display_name,
            protocol,
        } = self;
        f.debug_struct("DeviceRedeemParams")
            .field("invite_id", invite_id)
            .field("invite_secret", &crate::Redacted)
            .field("display_name", display_name)
            .field("protocol", protocol)
            .finish()
    }
}

impl fmt::Debug for DeviceRedeemResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            device_id,
            device_token: _,
            scope,
            expires_at_ms,
            host_id,
        } = self;
        f.debug_struct("DeviceRedeemResult")
            .field("device_id", device_id)
            .field("device_token", &crate::Redacted)
            .field("scope", scope)
            .field("expires_at_ms", expires_at_ms)
            .field("host_id", host_id)
            .finish()
    }
}

impl fmt::Debug for DeviceInviteCreateResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            offer: _,
            invite_id,
            expires_at_ms,
        } = self;
        f.debug_struct("DeviceInviteCreateResult")
            .field("offer", &crate::Redacted)
            .field("invite_id", invite_id)
            .field("expires_at_ms", expires_at_ms)
            .finish()
    }
}

/// `device/list` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceListResult {
    /// Every device, revoked ones included.
    pub devices: Vec<DeviceRow>,
    /// The registry version, the fence for revoke and rescope.
    pub version: i64,
}

/// `device/revoke` params, fenced on the registry version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRevokeParams {
    /// The device.
    pub device_id: String,
    /// The D18 envelope, flattened; `fence` is `registry_version`.
    #[serde(flatten)]
    pub mutation: MutationEnvelope,
}

/// `device/rescope` params, fenced on the registry version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRescopeParams {
    /// The device.
    pub device_id: String,
    /// The new scope. Never above the caller's own.
    pub scope: DeviceScope,
    /// The D18 envelope, flattened; `fence` is `registry_version`.
    #[serde(flatten)]
    pub mutation: MutationEnvelope,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn scope_wire_shape_is_frozen() {
        assert_eq!(
            serde_json::to_value(DeviceScope::MOBILE_TYPE).unwrap(),
            json!({"base":"mobile+type","admin":false})
        );
        assert_eq!(
            serde_json::to_value(DeviceScope::DESKTOP_ADMIN).unwrap(),
            json!({"base":"desktop","admin":true})
        );
        let bare: DeviceScope = serde_json::from_value(json!({"base":"mobile"})).unwrap();
        assert_eq!(bare, DeviceScope::MOBILE);
    }

    /// S1: admin implies base desktop, in the constructor and the decoder.
    #[test]
    fn admin_is_desktop_only() {
        for base in [BaseScope::Mobile, BaseScope::MobileType] {
            assert_eq!(
                DeviceScope::new(base, true),
                Err(ScopeError::AdminNeedsDesktop(base))
            );
            let wire = json!({"base": base.as_str(), "admin": true});
            assert!(serde_json::from_value::<DeviceScope>(wire).is_err());
        }
        assert_eq!(
            DeviceScope::new(BaseScope::Desktop, true),
            Ok(DeviceScope::DESKTOP_ADMIN)
        );
    }

    #[test]
    fn covers_follows_the_lattice() {
        let order = [
            DeviceScope::MOBILE,
            DeviceScope::MOBILE_TYPE,
            DeviceScope::DESKTOP,
            DeviceScope::DESKTOP_ADMIN,
        ];
        for (i, a) in order.iter().enumerate() {
            for (j, b) in order.iter().enumerate() {
                assert_eq!(a.covers(*b), i >= j, "{a:?} covers {b:?}");
            }
        }
    }

    /// The envelope is flattened on every new mutating params struct.
    #[test]
    fn device_mutations_flatten_the_envelope() {
        let wire = json!({
            "device_id": "d1",
            "op_id": "op-1",
            "fence": {"kind": "registry_version", "version": 3},
        });
        let revoke: DeviceRevokeParams = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(revoke.mutation.op_id.as_ref().unwrap().as_str(), "op-1");
        assert_eq!(serde_json::to_value(&revoke).unwrap(), wire);
    }

    #[test]
    fn device_list_result_is_frozen() {
        let result = DeviceListResult {
            devices: vec![DeviceRow {
                device_id: "d1".to_string(),
                display_name: "phone".to_string(),
                scope: DeviceScope::MOBILE,
                created_at_ms: 1,
                expires_at_ms: 2,
                last_seen_at_ms: 3,
                revoked_at_ms: None,
                static_pubkey_digest: "ab".to_string(),
            }],
            version: 7,
        };
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({"devices":[{"device_id":"d1","display_name":"phone",
                "scope":{"base":"mobile","admin":false},"created_at_ms":1,
                "expires_at_ms":2,"last_seen_at_ms":3,"static_pubkey_digest":"ab"}],
                "version":7})
        );
    }

    #[test]
    fn redeem_shapes_are_frozen() {
        let params = json!({
            "invite_id": "01K5A0000000000000000ABCDE",
            "invite_secret": "c2VjcmV0",
            "display_name": "phone",
            "protocol": {"min": 1, "max": 1},
        });
        let parsed: DeviceRedeemParams = serde_json::from_value(params.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), params);
        let result = json!({
            "device_id": "d1",
            "device_token": "mdd_x",
            "scope": {"base": "mobile", "admin": false},
            "expires_at_ms": 9,
            "host_id": "01K5A0000000000000000ABCDE",
        });
        let parsed: DeviceRedeemResult = serde_json::from_value(result.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), result);
        assert!(parsed.device_token.starts_with(DEVICE_TOKEN_PREFIX));
    }

    #[test]
    fn a_params_rule_needs_typed_params() {
        let attach: TerminalAttachParams = serde_json::from_value(json!({
            "session": {"host_id": "local", "session_key": "k"},
        }))
        .unwrap();
        let mut typing = attach.clone();
        typing.want_input = true;
        let phone = DeviceScope::MOBILE;
        assert!(method_allowed(
            &phone,
            m::TERMINAL_ATTACH,
            &CallParams::TerminalAttach(&attach)
        ));
        // S5: a read-only phone may not take the floor through attach.
        assert!(!method_allowed(
            &phone,
            m::TERMINAL_ATTACH,
            &CallParams::TerminalAttach(&typing)
        ));
        assert!(method_allowed(
            &DeviceScope::MOBILE_TYPE,
            m::TERMINAL_ATTACH,
            &CallParams::TerminalAttach(&typing)
        ));
        // C5: interrupt only, and only when the dispatcher parsed the action.
        assert!(method_allowed(
            &phone,
            m::FLEET_ACTION,
            &CallParams::FleetAction(&ControlAction::Interrupt)
        ));
        assert!(!method_allowed(
            &phone,
            m::FLEET_ACTION,
            &CallParams::Untyped
        ));
        assert!(!method_allowed(
            &phone,
            m::TERMINAL_ATTACH,
            &CallParams::Untyped
        ));
    }

    #[test]
    fn an_unclassified_method_is_refused_everywhere() {
        for column in ScopeColumn::ALL {
            assert!(!column_allows(column, "made/up", &CallParams::Untyped));
        }
    }

    #[test]
    fn mobile_event_families_are_fleet_attention_transcript() {
        let got: Vec<EventFamily> = EventFamily::ALL
            .into_iter()
            .filter(|f| event_allowed(&DeviceScope::MOBILE, *f))
            .collect();
        assert_eq!(
            got,
            vec![
                EventFamily::Attention,
                EventFamily::Fleet,
                EventFamily::Transcript
            ]
        );
        assert!(!event_allowed(
            &DeviceScope::DESKTOP,
            EventFamily::Connections
        ));
        assert!(!event_allowed(&DeviceScope::DESKTOP, EventFamily::Devices));
        assert!(event_allowed(
            &DeviceScope::DESKTOP_ADMIN,
            EventFamily::Connections
        ));
    }
}
