//! The shared in-memory [`MatrixDriver`] fake.
//!
//! The type and its constructors live here; the single required trait
//! implementation lives in [`matrix`].

mod matrix;

use std::collections::{HashMap, HashSet};
use tokio::sync::Mutex;

use cumments_core::models::{MatrixEvent, SiteId, VisitorProfile};
use cumments_core::poll::{PollSemanticAnswer, PollSemanticKind};
use cumments_core::profile::ProfileDriverError;

/// A recorded [`cumments_core::ports::MatrixDriver::post_poll_response`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedPollResponse {
    pub room_id: String,
    pub poll_event_id: String,
    /// Canonical selections emitted on the wire.
    pub option_ids: Vec<String>,
    pub operation_id: String,
    pub txn_id: String,
}

/// A recorded [`cumments_core::ports::MatrixDriver::post_poll_end`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedPollEnd {
    pub room_id: String,
    pub poll_event_id: String,
    pub operation_id: String,
    pub txn_id: String,
}

/// A recorded [`cumments_core::ports::MatrixDriver::post_poll`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedPoll {
    pub room_id: String,
    pub question: String,
    pub answers: Vec<PollSemanticAnswer>,
    pub kind: PollSemanticKind,
    pub max_selections: u64,
    pub operation_id: String,
    pub submission_id: Option<i64>,
    pub txn_id: String,
}

/// In-memory [`MatrixDriver`] double that records the calls each test
/// asserts.
///
/// Methods outside the exercised surface panic with `unimplemented!()` so a
/// test that accidentally depends on untracked driver behavior fails loudly
/// instead of silently passing.
pub struct TestDriver {
    pub joined: Mutex<Vec<String>>,
    pub joined_members: Mutex<Vec<String>>,
    pub joined_member_queries: Mutex<Vec<String>>,
    pub left: Mutex<Vec<String>>,
    pub left_as: Mutex<Vec<(String, String)>>,
    pub replies: Mutex<Vec<(String, String)>>,
    pub deleted: Mutex<Vec<(String, String)>>,
    pub joined_rooms: Mutex<Vec<String>>,
    pub room_events: Mutex<HashMap<String, Vec<serde_json::Value>>>,
    pub events: Mutex<HashMap<(String, String), MatrixEvent>>,
    pub room_metadata: Mutex<HashMap<String, serde_json::Value>>,
    pub room_state: Mutex<HashMap<(String, String, String), serde_json::Value>>,
    pub state_writes: Mutex<Vec<(String, String, String)>>,
    pub power_levels: Mutex<HashMap<String, serde_json::Value>>,
    pub upgrades: Mutex<Vec<(String, String)>>,
    pub adoptions: Mutex<Vec<String>>,
    pub space_links: Mutex<Vec<(String, String)>>,
    pub invites: Mutex<Vec<(String, String)>>,
    pub reactions: Mutex<Vec<(String, String, String, String)>>,
    pub poll_responses: Mutex<Vec<RecordedPollResponse>>,
    pub poll_ends: Mutex<Vec<RecordedPollEnd>>,
    /// Recorded `post_poll` calls, richest-first so tests can assert the
    /// structured semantic payload the reconciler handed the driver.
    pub polls: Mutex<Vec<RecordedPoll>>,
    pub avatar_updates: Mutex<Vec<(String, String, Option<String>)>>,
    pub visitor_profiles: Mutex<HashMap<(String, String), VisitorProfile>>,
    pub redactions: Mutex<Vec<(String, String, String)>>,
    /// Rooms for which `join_room` should fail (test-only injection).
    pub fail_join_rooms: Mutex<HashSet<String>>,
    /// Fail the next N `post_poll_response` calls with a simulated transport error
    /// before Matrix accepts the event.
    pub fail_poll_response_count: Mutex<usize>,
    /// Record the next N `post_poll_response` calls into `poll_responses` (homeserver
    /// accepted) but return an error (client response lost).
    pub ambiguous_poll_response_count: Mutex<usize>,
    /// Fail the next N `post_poll_end` calls with a simulated transport error
    /// before Matrix accepts the event.
    pub fail_poll_end_count: Mutex<usize>,
    /// Record the next N `post_poll_end` calls into `poll_ends` (homeserver
    /// accepted) but return an error (client response lost).
    pub ambiguous_poll_end_count: Mutex<usize>,
    pub set_display_name_calls: Mutex<Vec<(String, SiteId, String)>>,
    pub clear_display_name_calls: Mutex<Vec<(String, SiteId)>>,
    pub set_avatar_calls: Mutex<Vec<(String, SiteId, String)>>,
    pub clear_avatar_calls: Mutex<Vec<(String, SiteId)>>,
    pub next_profile_error: Mutex<Option<ProfileDriverError>>,
}

impl TestDriver {
    pub fn new() -> Self {
        Self {
            joined: Mutex::new(Vec::new()),
            joined_members: Mutex::new(Vec::new()),
            joined_member_queries: Mutex::new(Vec::new()),
            left: Mutex::new(Vec::new()),
            left_as: Mutex::new(Vec::new()),
            replies: Mutex::new(Vec::new()),
            deleted: Mutex::new(Vec::new()),
            joined_rooms: Mutex::new(Vec::new()),
            room_events: Mutex::new(HashMap::new()),
            events: Mutex::new(HashMap::new()),
            room_metadata: Mutex::new(HashMap::new()),
            room_state: Mutex::new(HashMap::new()),
            state_writes: Mutex::new(Vec::new()),
            power_levels: Mutex::new(HashMap::new()),
            upgrades: Mutex::new(Vec::new()),
            adoptions: Mutex::new(Vec::new()),
            space_links: Mutex::new(Vec::new()),
            invites: Mutex::new(Vec::new()),
            reactions: Mutex::new(Vec::new()),
            poll_responses: Mutex::new(Vec::new()),
            poll_ends: Mutex::new(Vec::new()),
            polls: Mutex::new(Vec::new()),
            avatar_updates: Mutex::new(Vec::new()),
            visitor_profiles: Mutex::new(HashMap::new()),
            redactions: Mutex::new(Vec::new()),
            fail_join_rooms: Mutex::new(HashSet::new()),
            fail_poll_response_count: Mutex::new(0),
            ambiguous_poll_response_count: Mutex::new(0),
            fail_poll_end_count: Mutex::new(0),
            ambiguous_poll_end_count: Mutex::new(0),
            set_display_name_calls: Mutex::new(Vec::new()),
            clear_display_name_calls: Mutex::new(Vec::new()),
            set_avatar_calls: Mutex::new(Vec::new()),
            clear_avatar_calls: Mutex::new(Vec::new()),
            next_profile_error: Mutex::new(None),
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

    pub fn with_event(mut self, event: MatrixEvent) -> Self {
        self.events
            .get_mut()
            .insert((event.room_id.clone(), event.event_id.clone()), event);
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

    pub async fn set_next_profile_error(&self, error: cumments_core::profile::ProfileDriverError) {
        *self.next_profile_error.lock().await = Some(error);
    }

    pub async fn insert_visitor_profile(
        &self,
        site_id: impl Into<String>,
        author_public_key: impl Into<String>,
        profile: VisitorProfile,
    ) {
        self.visitor_profiles
            .lock()
            .await
            .insert((site_id.into(), author_public_key.into()), profile);
    }
}

impl Default for TestDriver {
    fn default() -> Self {
        Self::new()
    }
}
