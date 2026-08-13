//! Ephemeral room events (typing / read receipts / presence) pushed to
//! subscribed clients over SSE.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// A live ephemeral event for a room.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EphemeralEvent {
    /// A user started or stopped typing in a room.
    Typing {
        room_id: String,
        user_id: String,
        typing: bool,
        /// Display name snapshot when known (from `room_members`).
        #[serde(skip_serializing_if = "Option::is_none")]
        display_name: Option<String>,
    },
    /// A user read up to a message in a room (public read receipts only).
    ReadReceipt {
        room_id: String,
        event_id: String,
        user_id: String,
    },
    /// A user's presence changed (when the homeserver exposes it).
    Presence { user_id: String, presence: String },
}

/// In-memory typing state so new SSE subscribers get a snapshot.
#[derive(Default)]
pub struct EphemeralState {
    typing: Mutex<HashMap<String, HashSet<String>>>,
}

impl EphemeralState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn apply(&self, event: &EphemeralEvent) {
        if let EphemeralEvent::Typing {
            room_id,
            user_id,
            typing,
            ..
        } = event
        {
            let mut map = self.typing.lock().unwrap_or_else(|e| e.into_inner());
            let users = map.entry(room_id.clone()).or_default();
            if *typing {
                users.insert(user_id.clone());
            } else {
                users.remove(user_id);
            }
            if users.is_empty() {
                map.remove(room_id);
            }
        }
    }

    /// Users currently typing in a room (for initial SSE snapshots).
    pub fn typing_snapshot(&self, room_id: &str) -> Vec<String> {
        self.typing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(room_id)
            .map(|users| users.iter().cloned().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_state_tracks_users_and_snapshots() {
        let state = EphemeralState::new();
        state.apply(&EphemeralEvent::Typing {
            room_id: "!room:hs".to_string(),
            user_id: "@alice:hs".to_string(),
            typing: true,
            display_name: None,
        });
        state.apply(&EphemeralEvent::Typing {
            room_id: "!room:hs".to_string(),
            user_id: "@bob:hs".to_string(),
            typing: true,
            display_name: None,
        });
        let mut snapshot = state.typing_snapshot("!room:hs");
        snapshot.sort();
        assert_eq!(
            snapshot,
            vec!["@alice:hs".to_string(), "@bob:hs".to_string()]
        );

        state.apply(&EphemeralEvent::Typing {
            room_id: "!room:hs".to_string(),
            user_id: "@alice:hs".to_string(),
            typing: false,
            display_name: None,
        });
        assert_eq!(
            state.typing_snapshot("!room:hs"),
            vec!["@bob:hs".to_string()]
        );
        assert!(state.typing_snapshot("!other:hs").is_empty());
    }
}
