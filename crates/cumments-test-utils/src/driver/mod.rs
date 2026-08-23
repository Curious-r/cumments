//! The shared in-memory [`MatrixDriver`] fake.
//!
//! The type and its constructors live here; the single required trait
//! implementation lives in [`matrix`].

mod matrix;

use std::collections::HashMap;
use tokio::sync::Mutex;

use cumments_core::models::VisitorProfile;

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
    pub upgrades: Mutex<Vec<(String, String)>>,
    pub adoptions: Mutex<Vec<String>>,
    pub space_links: Mutex<Vec<(String, String)>>,
    pub invites: Mutex<Vec<(String, String)>>,
    pub reactions: Mutex<Vec<(String, String, String, String)>>,
    pub poll_votes: Mutex<Vec<(String, String, String, String)>>,
    pub avatar_updates: Mutex<Vec<(String, String, Option<String>)>>,
    pub visitor_profiles: Mutex<HashMap<(String, String), VisitorProfile>>,
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
            upgrades: Mutex::new(Vec::new()),
            adoptions: Mutex::new(Vec::new()),
            space_links: Mutex::new(Vec::new()),
            invites: Mutex::new(Vec::new()),
            reactions: Mutex::new(Vec::new()),
            poll_votes: Mutex::new(Vec::new()),
            avatar_updates: Mutex::new(Vec::new()),
            visitor_profiles: Mutex::new(HashMap::new()),
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

    pub fn with_visitor_profile(
        mut self,
        site_id: impl Into<String>,
        author_public_key: impl Into<String>,
        profile: VisitorProfile,
    ) -> Self {
        self.visitor_profiles
            .get_mut()
            .insert((site_id.into(), author_public_key.into()), profile);
        self
    }
}

impl Default for TestDriver {
    fn default() -> Self {
        Self::new()
    }
}
