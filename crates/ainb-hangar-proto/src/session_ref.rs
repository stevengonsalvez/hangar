//! A session named across hosts (spec D11).
//!
//! Frozen by PR-0.
//!
//! `session_key` is the key space of `fleet_session.session_key`: the D14
//! store key, and the key a phone reads from `fleet/roster_status`. It is NOT
//! `sessions.session_id` or a tmux session name. The daemon resolves the pane
//! through the fleet pane binding (`fleet_session.tmux_target`).

use serde::{Deserialize, Serialize};

use crate::hosts::HostId;

/// One session on one host.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionRef {
    /// The host that owns the session.
    pub host_id: HostId,
    /// The session's `fleet_session.session_key` on that host.
    pub session_key: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_shape_is_frozen() {
        let session = SessionRef {
            host_id: HostId::parse("01K5A0000000000000000ABCDE").unwrap(),
            session_key: "claude:abc".to_string(),
        };
        let wire = serde_json::json!({
            "host_id": "01K5A0000000000000000ABCDE",
            "session_key": "claude:abc",
        });
        assert_eq!(serde_json::to_value(&session).unwrap(), wire);
        assert_eq!(serde_json::from_value::<SessionRef>(wire).unwrap(), session);
    }

    #[test]
    fn a_bad_host_id_refuses_the_whole_ref() {
        let wire = serde_json::json!({"host_id": "h1", "session_key": "k"});
        assert!(serde_json::from_value::<SessionRef>(wire).is_err());
    }
}
