//! Wake every `fleet/subscribe` reader when a human closes or reopens an
//! attention row.
//!
//! The agent-status read says `waiting` only while the inbox holds an open
//! request for the session (#962), so a fresh read reflects an answer at once.
//! But a reader re-reads only on a new fleet revision, and closing an
//! attention row writes no fleet event. A driven run showed the desktop board
//! keeping an answered ask for 20 s to 3 min after delivery, until the
//! agent's next hook happened to write one.
//!
//! ```text
//!  answer ──closes row──▶ attention ──(no revision)──▶ reader never re-reads
//!  answer ──closes row──▶ wake ──no-op fleet event──▶ revision ──▶ re-read
//! ```
//!
//! The event carries an empty patch: it changes no session state, bumps no
//! session version and moves no `last_observed_at`. Its only effect is the
//! revision every reader already follows, the same contract the ACP answer
//! paths keep (`acp_pool.rs`).

use ainb_hangar_store::repo::fleet::{
    FleetRepo, FleetSessionPatch, NewFleetEvent, ObservationAuthority,
};
use sqlx::SqlitePool;

use crate::events::EventSink;

/// What happened to the attention row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionChange {
    /// A human answered or dismissed it.
    Closed,
    /// Its delivery failed and the claim was reverted to open.
    Reopened,
}

impl AttentionChange {
    const fn event_type(self) -> &'static str {
        match self {
            Self::Closed => "attention_closed",
            Self::Reopened => "attention_reopened",
        }
    }
}

/// Wake readers for every visible fleet row registered under
/// `provider_session_id` (an attention row's `session_id`). A session no
/// fleet row names has no card to refresh, so nothing is written.
pub async fn wake_for_provider_session(
    pool: &SqlitePool,
    events: &EventSink,
    provider_session_id: &str,
    attention_id: &str,
    change: AttentionChange,
    now_ms: i64,
) {
    let keys = match FleetRepo::session_keys_for_provider_session(pool, provider_session_id).await {
        Ok(keys) => keys,
        Err(error) => {
            tracing::warn!(%error, attention_id, "attention wake: fleet row lookup failed");
            return;
        }
    };
    for key in keys {
        wake(pool, events, &key, attention_id, change, now_ms).await;
    }
}

/// Wake readers for the fleet row `session_key`, which must already exist:
/// an event for an unknown key would create a row. Private so every caller
/// goes through [`wake_for_provider_session`], which only names rows that do.
async fn wake(
    pool: &SqlitePool,
    events: &EventSink,
    session_key: &str,
    attention_id: &str,
    change: AttentionChange,
    now_ms: i64,
) {
    let event = NewFleetEvent {
        // The time is part of the id: the same row can close, reopen on a
        // failed delivery and close again, and each of those must wake.
        event_id: format!(
            "{}:{attention_id}:{session_key}:{now_ms}",
            change.event_type()
        ),
        session_key: session_key.to_string(),
        observed_at: now_ms,
        authority: ObservationAuthority::Authoritative,
        event_type: change.event_type().to_string(),
        payload: serde_json::json!({ "attention_id": attention_id }).to_string(),
        patch: FleetSessionPatch::default(),
    };
    match FleetRepo::apply_event(pool, &event).await {
        Ok(result) if !result.duplicate => events.emit_fleet_revision(result.revision),
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(%session_key, %error, attention_id, "attention wake: fleet event failed");
        }
    }
}
