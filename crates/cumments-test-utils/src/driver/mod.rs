//! The shared in-memory [`MatrixDriver`] fake.
//!
//! The type and its constructors live here; the single required trait
//! implementation lives in [`matrix`].

mod matrix;

use std::collections::HashMap;
use tokio::sync::Mutex;

/// In-memory [`MatrixDriver`] double that records the calls each test
/// asserts.
///
/// Methods outside the exercised surface panic with `unimplemented!()` so a
/// test that accidentally depends on untracked driver behavior fails loudly
/// instead of silently passing.
pub struct TestDriver {
    pub joined: Mutex<Vec<String>>,
    pub joined_members: Mutex<Vec<String>>,
    pub left: Mutex<Vec<String>>,
    pub left_as: Mutex<Vec<(String, String)>>,
    pub replies: Mutex<Vec<(String, String)>>,
    pub deleted: Mutex<Vec<(String, String)>>,
    pub joined_rooms: Mutex<Vec<String>>,
    pub room_events: Mutex<HashMap<String, Vec<serde_json::Value>>>,
    pub room_metadata: Mutex<HashMap<String, serde_json::Value>>,
    pub room_state: Mutex<HashMap<(String, String, String), serde_json::Value>>,
    pub state_writes: Mutex<Vec<(String, String, String)>>,
    pub power_levels: Mutex<HashMap<String, serde_json::Value>>,
}

impl TestDriver {
    pub fn new() -> Self {
        Self {
            joined: Mutex::new(Vec::new()),
            joined_members: Mutex::new(Vec::new()),
            left: Mutex::new(Vec::new()),
            left_as: Mutex::new(Vec::new()),
            replies: Mutex::new(Vec::new()),
            deleted: Mutex::new(Vec::new()),
            joined_rooms: Mutex::new(Vec::new()),
            room_events: Mutex::new(HashMap::new()),
            room_metadata: Mutex::new(HashMap::new()),
            room_state: Mutex::new(HashMap::new()),
            state_writes: Mutex::new(Vec::new()),
            power_levels: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_joined_members(members: Vec<String>) -> Self {
        Self {
            joined_members: Mutex::new(members),
            ..Self::new()
        }
    }

    pub fn with_joined_rooms(rooms: Vec<String>) -> Self {
        Self {
            joined_rooms: Mutex::new(rooms),
            ..Self::new()
        }
    }

    pub fn with_room_events(
        mut self,
        room_id: impl Into<String>,
        events: Vec<serde_json::Value>,
    ) -> Self {
        self.room_events.get_mut().insert(room_id.into(), events);
        self
    }

    pub fn with_room_metadata(
        mut self,
        room_id: impl Into<String>,
        metadata: serde_json::Value,
    ) -> Self {
        self.room_metadata
            .get_mut()
            .insert(room_id.into(), metadata);
        self
    }

    pub fn with_room_state(
        mut self,
        room_id: impl Into<String>,
        event_type: impl Into<String>,
        state_key: impl Into<String>,
        content: serde_json::Value,
    ) -> Self {
        self.room_state.get_mut().insert(
            (room_id.into(), event_type.into(), state_key.into()),
            content,
        );
        self
    }

    pub fn with_power_levels(
        mut self,
        room_id: impl Into<String>,
        content: serde_json::Value,
    ) -> Self {
        self.power_levels.get_mut().insert(room_id.into(), content);
        self
    }
}

impl Default for TestDriver {
    fn default() -> Self {
        Self::new()
    }
}
