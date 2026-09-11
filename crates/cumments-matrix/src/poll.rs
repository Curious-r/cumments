//! Matrix-side poll representation for the pinned MSC3381 revision.
//!
//! This module models only the Matrix wire representation of polls at the
//! historical MSC3381 revision adopted by Cumments
//! (`e5eb721d37f5c1f54a70b67eca3f5118a666259b`), using the unstable
//! `org.matrix.msc3381.poll.*` direct event types (not the `m.room.message`
//! wrapper and not the current stable `m.poll.*` types).
//!
//! It is deliberately transport agnostic: it parses inbound Matrix event
//! content into typed domain values and builds outbound Matrix event content.
//! It never performs HTTP, signing, proof-of-work, operation identity,
//! persistence or projection. The Cumments semantic operations
//! (`POLL` / `VOTE` / `END_POLL`), semantic fingerprints, signatures and
//! `operation_id` handling live in later layers and are intentionally absent.
//!
//! Wire reference: MSC3381 "Unstable prefix" section at the pinned revision,
//! which defines for `org.matrix.msc3381.poll.start` a single-string
//! `org.matrix.msc1767.text` fallback, `kind` values prefixed with
//! `org.matrix.msc3381.poll.`, `id` (unprefixed) on answers, and a required
//! empty `org.matrix.msc3381.poll.end` object on end events.

use cumments_core::models::MatrixEvent;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Event type of a poll start event.
pub const POLL_START_EVENT_TYPE: &str = "org.matrix.msc3381.poll.start";

/// Event type of a poll response (vote) event.
pub const POLL_RESPONSE_EVENT_TYPE: &str = "org.matrix.msc3381.poll.response";

/// Event type of a poll end event.
pub const POLL_END_EVENT_TYPE: &str = "org.matrix.msc3381.poll.end";

/// Content key carrying the `poll.start` block.
pub const POLL_START_CONTENT_KEY: &str = "org.matrix.msc3381.poll.start";

/// Content key carrying the `poll.response` block.
pub const POLL_RESPONSE_CONTENT_KEY: &str = "org.matrix.msc3381.poll.response";

/// Content key carrying the `poll.end` block.
pub const POLL_END_CONTENT_KEY: &str = "org.matrix.msc3381.poll.end";

/// Content key of the extensible-events single-string text fallback.
pub const MSC1767_TEXT_KEY: &str = "org.matrix.msc1767.text";

/// Reference relation type used by poll responses and ends.
pub const REFERENCE_RELATION_TYPE: &str = "m.reference";

/// The `kind` value for a disclosed poll.
pub const POLL_KIND_DISCLOSED: &str = "org.matrix.msc3381.poll.disclosed";

/// The `kind` value for an undisclosed poll.
pub const POLL_KIND_UNDISCLOSED: &str = "org.matrix.msc3381.poll.undisclosed";

/// Whether an event type is one of the adopted MSC3381 poll event types.
pub fn is_poll_event_type(event_type: &str) -> bool {
    matches!(
        event_type,
        POLL_START_EVENT_TYPE | POLL_RESPONSE_EVENT_TYPE | POLL_END_EVENT_TYPE
    )
}

/// A poll's general approach, as carried in the `kind` field.
///
/// MSC3381 permits custom namespaced kinds; clients which do not recognize a
/// kind must treat it as `undisclosed`. Known values are modelled explicitly
/// and unknown values are preserved verbatim in [`PollKind::Other`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PollKind {
    /// `org.matrix.msc3381.poll.disclosed`.
    Disclosed,
    /// `org.matrix.msc3381.poll.undisclosed` (also the default).
    #[default]
    Undisclosed,
    /// Any other namespaced kind, preserved as authored.
    Other(String),
}

impl PollKind {
    /// The wire value for this kind.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Disclosed => POLL_KIND_DISCLOSED,
            Self::Undisclosed => POLL_KIND_UNDISCLOSED,
            Self::Other(kind) => kind,
        }
    }

    /// Interpret a wire `kind` value, defaulting unknown kinds to
    /// [`PollKind::Other`] (which clients treat as undisclosed).
    pub fn from_wire(kind: &str) -> Self {
        match kind {
            POLL_KIND_DISCLOSED => Self::Disclosed,
            POLL_KIND_UNDISCLOSED => Self::Undisclosed,
            other => Self::Other(other.to_string()),
        }
    }

    /// Whether results may be shown while the poll is still open.
    pub fn is_disclosed(&self) -> bool {
        matches!(self, Self::Disclosed)
    }
}

impl Serialize for PollKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PollKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let kind = String::deserialize(deserializer)?;
        Ok(Self::from_wire(&kind))
    }
}

/// A reference relation (`m.relates_to` with `rel_type: m.reference`), used by
/// poll responses and ends to point at the poll start event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceRelation {
    #[serde(rename = "rel_type")]
    pub rel_type: ReferenceRelType,
    pub event_id: String,
}

impl ReferenceRelation {
    /// A reference to `event_id`.
    pub fn reference(event_id: impl Into<String>) -> Self {
        Self {
            rel_type: ReferenceRelType::Reference,
            event_id: event_id.into(),
        }
    }
}

/// The only relation type a poll response or end may carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReferenceRelType {
    #[serde(rename = "m.reference")]
    Reference,
}

/// The `question` object of a poll start block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollQuestion {
    #[serde(rename = "org.matrix.msc1767.text")]
    pub text: String,
}

impl PollQuestion {
    /// A question with the given text.
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

/// One answer option of a poll start block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollAnswer {
    /// Client-generated opaque answer identifier.
    pub id: String,
    #[serde(rename = "org.matrix.msc1767.text")]
    pub text: String,
}

impl PollAnswer {
    /// An answer with the given opaque `id` and display `text`.
    pub fn new(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
        }
    }
}

/// The `org.matrix.msc3381.poll.start` block body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollStartBody {
    /// Optional on the wire; absent means `undisclosed`.
    #[serde(default)]
    pub kind: PollKind,
    /// Optional on the wire; absent means `1`.
    #[serde(default = "default_max_selections")]
    pub max_selections: u64,
    pub question: PollQuestion,
    pub answers: Vec<PollAnswer>,
}

/// The full content of a `org.matrix.msc3381.poll.start` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollStartContent {
    #[serde(rename = "org.matrix.msc1767.text")]
    pub fallback_text: String,
    #[serde(rename = "org.matrix.msc3381.poll.start")]
    pub poll: PollStartBody,
}

impl PollStartContent {
    /// Build a start content value from poll wire inputs.
    pub fn new(
        fallback_text: impl Into<String>,
        question: impl Into<String>,
        answers: Vec<PollAnswer>,
        kind: PollKind,
        max_selections: u64,
    ) -> Self {
        Self {
            fallback_text: fallback_text.into(),
            poll: PollStartBody {
                kind,
                max_selections,
                question: PollQuestion::new(question),
                answers,
            },
        }
    }
}

/// The `org.matrix.msc3381.poll.response` block body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollResponseBody {
    /// Selected answer identifiers; empty denotes an explicit unvote.
    pub answers: Vec<String>,
}

/// The full content of a `org.matrix.msc3381.poll.response` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollResponseContent {
    #[serde(rename = "m.relates_to")]
    pub relates_to: ReferenceRelation,
    #[serde(rename = "org.matrix.msc3381.poll.response")]
    pub response: PollResponseBody,
}

impl PollResponseContent {
    /// Build a response content value referencing `poll_event_id`.
    pub fn new(poll_event_id: impl Into<String>, answers: Vec<String>) -> Self {
        Self {
            relates_to: ReferenceRelation::reference(poll_event_id),
            response: PollResponseBody { answers },
        }
    }
}

/// The required, empty `org.matrix.msc3381.poll.end` marker object.
///
/// The pinned revision requires the key to be present and its value to be an
/// empty object. Unknown keys are tolerated, matching the repository's
/// tolerant Matrix parsing conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollEndMarker {}

/// The full content of a `org.matrix.msc3381.poll.end` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollEndContent {
    #[serde(rename = "m.relates_to")]
    pub relates_to: ReferenceRelation,
    #[serde(rename = "org.matrix.msc1767.text")]
    pub fallback_text: String,
    #[serde(rename = "org.matrix.msc3381.poll.end")]
    pub end: PollEndMarker,
}

impl PollEndContent {
    /// Build an end content value referencing `poll_event_id`.
    pub fn new(poll_event_id: impl Into<String>, fallback_text: impl Into<String>) -> Self {
        Self {
            relates_to: ReferenceRelation::reference(poll_event_id),
            fallback_text: fallback_text.into(),
            end: PollEndMarker {},
        }
    }
}

/// A typed `org.matrix.msc3381.poll.start` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollStartEvent {
    /// The enclosing event's ID, when the representation provides one.
    pub event_id: Option<String>,
    pub sender: String,
    pub origin_server_ts: i64,
    pub content: PollStartContent,
}

/// A typed `org.matrix.msc3381.poll.response` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollResponseEvent {
    pub event_id: Option<String>,
    pub sender: String,
    pub origin_server_ts: i64,
    pub content: PollResponseContent,
}

/// A typed `org.matrix.msc3381.poll.end` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollEndEvent {
    pub event_id: Option<String>,
    pub sender: String,
    pub origin_server_ts: i64,
    pub content: PollEndContent,
}

/// Any typed poll event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollEvent {
    Start(PollStartEvent),
    Response(PollResponseEvent),
    End(PollEndEvent),
}

impl PollEvent {
    /// The Matrix event type this value maps to.
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::Start(_) => POLL_START_EVENT_TYPE,
            Self::Response(_) => POLL_RESPONSE_EVENT_TYPE,
            Self::End(_) => POLL_END_EVENT_TYPE,
        }
    }

    /// Parse a Matrix event into a typed poll event.
    ///
    /// Returns `Ok(None)` when `event_type` is not one of the adopted MSC3381
    /// poll event types; unrelated Matrix events are never reinterpreted as
    /// polls. Returns an error when `event_type` is a poll type but the
    /// content is malformed or missing required poll fields.
    pub fn parse(
        event_type: &str,
        event_id: Option<&str>,
        sender: &str,
        origin_server_ts: i64,
        content: &serde_json::Value,
    ) -> Result<Option<Self>, PollEventError> {
        let event = match event_type {
            POLL_START_EVENT_TYPE => {
                let content = PollStartContent::deserialize(content)?;
                content.validate()?;
                Self::Start(PollStartEvent {
                    event_id: event_id.map(str::to_owned),
                    sender: sender.to_owned(),
                    origin_server_ts,
                    content,
                })
            }
            POLL_RESPONSE_EVENT_TYPE => Self::Response(PollResponseEvent {
                event_id: event_id.map(str::to_owned),
                sender: sender.to_owned(),
                origin_server_ts,
                content: PollResponseContent::deserialize(content)?,
            }),
            POLL_END_EVENT_TYPE => Self::End(PollEndEvent {
                event_id: event_id.map(str::to_owned),
                sender: sender.to_owned(),
                origin_server_ts,
                content: PollEndContent::deserialize(content)?,
            }),
            _ => return Ok(None),
        };
        Ok(Some(event))
    }

    /// Parse a repository [`MatrixEvent`] into a typed poll event.
    ///
    /// Returns `Ok(None)` for non-poll event types.
    pub fn from_matrix_event(event: &MatrixEvent) -> Result<Option<Self>, PollEventError> {
        let Some(sender) = event.sender.as_deref() else {
            if is_poll_event_type(&event.event_type) {
                return Err(PollEventError::MissingSender);
            }
            return Ok(None);
        };
        Self::parse(
            &event.event_type,
            Some(&event.event_id),
            sender,
            event.origin_server_ts,
            &event.content,
        )
    }
}

impl PollStartContent {
    fn validate(&self) -> Result<(), PollEventError> {
        if self.poll.max_selections < 1 {
            return Err(PollEventError::Invalid(
                "max_selections must be at least 1".to_string(),
            ));
        }
        Ok(())
    }
}

/// Errors produced when parsing a Matrix poll event.
#[derive(Debug, thiserror::Error)]
pub enum PollEventError {
    /// A poll event's enclosing representation has no sender.
    #[error("poll event is missing a sender")]
    MissingSender,
    /// The content could not be deserialized into the pinned wire schema.
    #[error("malformed poll event content: {0}")]
    Malformed(#[from] serde_json::Error),
    /// The content deserialized but violates a protocol constraint.
    #[error("invalid poll event: {0}")]
    Invalid(String),
}

fn default_max_selections() -> u64 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn start_content() -> serde_json::Value {
        json!({
            "org.matrix.msc1767.text": "What should we order?\n1. Pizza\n2. Poutine",
            "org.matrix.msc3381.poll.start": {
                "kind": "org.matrix.msc3381.poll.disclosed",
                "max_selections": 1,
                "question": { "org.matrix.msc1767.text": "What should we order?" },
                "answers": [
                    { "id": "pizza", "org.matrix.msc1767.text": "Pizza" },
                    { "id": "poutine", "org.matrix.msc1767.text": "Poutine" },
                ],
            },
        })
    }

    fn response_content() -> serde_json::Value {
        json!({
            "m.relates_to": { "rel_type": "m.reference", "event_id": "$poll:hs" },
            "org.matrix.msc3381.poll.response": { "answers": ["poutine"] },
        })
    }

    fn end_content() -> serde_json::Value {
        json!({
            "m.relates_to": { "rel_type": "m.reference", "event_id": "$poll:hs" },
            "org.matrix.msc1767.text": "The poll has ended. Top answer: Pizza",
            "org.matrix.msc3381.poll.end": {},
        })
    }

    // ── Parsing ───────────────────────────────────────────────────

    #[test]
    fn parses_valid_poll_start() {
        let event = PollEvent::parse(
            POLL_START_EVENT_TYPE,
            Some("$start:hs"),
            "@alice:hs",
            1234,
            &start_content(),
        )
        .expect("valid start")
        .expect("is a poll event");

        let PollEvent::Start(start) = event else {
            panic!("expected start");
        };
        assert_eq!(start.event_id.as_deref(), Some("$start:hs"));
        assert_eq!(start.sender, "@alice:hs");
        assert_eq!(start.origin_server_ts, 1234);
        assert_eq!(start.content.poll.kind, PollKind::Disclosed);
        assert!(start.content.poll.kind.is_disclosed());
        assert_eq!(start.content.poll.max_selections, 1);
        assert_eq!(start.content.poll.question.text, "What should we order?");
        assert_eq!(start.content.poll.answers.len(), 2);
        assert_eq!(start.content.poll.answers[0].id, "pizza");
        assert_eq!(start.content.poll.answers[0].text, "Pizza");
        assert_eq!(start.content.poll.answers[1].id, "poutine");
        assert_eq!(
            start.content.fallback_text,
            "What should we order?\n1. Pizza\n2. Poutine"
        );
    }

    #[test]
    fn parses_valid_poll_response() {
        let event = PollEvent::parse(
            POLL_RESPONSE_EVENT_TYPE,
            Some("$vote:hs"),
            "@bob:hs",
            2345,
            &response_content(),
        )
        .expect("valid response")
        .expect("is a poll event");

        let PollEvent::Response(response) = event else {
            panic!("expected response");
        };
        assert_eq!(response.event_id.as_deref(), Some("$vote:hs"));
        assert_eq!(response.sender, "@bob:hs");
        assert_eq!(response.origin_server_ts, 2345);
        assert_eq!(response.content.relates_to.event_id, "$poll:hs");
        assert_eq!(response.content.response.answers, vec!["poutine"]);
    }

    #[test]
    fn parses_valid_poll_end() {
        let event = PollEvent::parse(
            POLL_END_EVENT_TYPE,
            Some("$end:hs"),
            "@alice:hs",
            3456,
            &end_content(),
        )
        .expect("valid end")
        .expect("is a poll event");

        let PollEvent::End(end) = event else {
            panic!("expected end");
        };
        assert_eq!(end.event_id.as_deref(), Some("$end:hs"));
        assert_eq!(end.sender, "@alice:hs");
        assert_eq!(end.origin_server_ts, 3456);
        assert_eq!(end.content.relates_to.event_id, "$poll:hs");
        assert_eq!(
            end.content.fallback_text,
            "The poll has ended. Top answer: Pizza"
        );
        assert_eq!(end.content.end, PollEndMarker {});
    }

    #[test]
    fn non_poll_event_types_are_not_reinterpreted() {
        // The legacy `m.room.message` wrapper and the current stable `m.poll.*`
        // types must all be left to their own layers.
        for event_type in [
            "m.room.message",
            "m.reaction",
            "m.poll.start",
            "m.poll.response",
            "m.poll.end",
        ] {
            let parsed =
                PollEvent::parse(event_type, Some("$e:hs"), "@alice:hs", 1, &start_content())
                    .expect("non-poll event types never error");
            assert!(parsed.is_none(), "{event_type} must not parse as a poll");
        }
    }

    #[test]
    fn rejects_missing_required_start_fields() {
        // Missing the poll.start content key.
        let no_block = json!({ "org.matrix.msc1767.text": "fallback" });
        assert!(matches!(
            PollEvent::parse(POLL_START_EVENT_TYPE, None, "@a:hs", 1, &no_block),
            Err(PollEventError::Malformed(_))
        ));

        // Missing the question.
        let no_question = json!({
            "org.matrix.msc1767.text": "fallback",
            "org.matrix.msc3381.poll.start": { "answers": [{ "id": "a", "org.matrix.msc1767.text": "A" }] },
        });
        assert!(PollEvent::parse(POLL_START_EVENT_TYPE, None, "@a:hs", 1, &no_question).is_err());

        // Missing an answer id.
        let no_answer_id = json!({
            "org.matrix.msc1767.text": "fallback",
            "org.matrix.msc3381.poll.start": {
                "question": { "org.matrix.msc1767.text": "Q" },
                "answers": [{ "org.matrix.msc1767.text": "A" }],
            },
        });
        assert!(PollEvent::parse(POLL_START_EVENT_TYPE, None, "@a:hs", 1, &no_answer_id).is_err());

        // Missing an answer text.
        let no_answer_text = json!({
            "org.matrix.msc1767.text": "fallback",
            "org.matrix.msc3381.poll.start": {
                "question": { "org.matrix.msc1767.text": "Q" },
                "answers": [{ "id": "a" }],
            },
        });
        assert!(
            PollEvent::parse(POLL_START_EVENT_TYPE, None, "@a:hs", 1, &no_answer_text).is_err()
        );
    }

    #[test]
    fn rejects_missing_required_response_fields() {
        // Missing the response content key.
        let no_block = json!({
            "m.relates_to": { "rel_type": "m.reference", "event_id": "$p:hs" },
        });
        assert!(PollEvent::parse(POLL_RESPONSE_EVENT_TYPE, None, "@a:hs", 1, &no_block).is_err());

        // Missing the whole relation.
        let no_relation = json!({
            "org.matrix.msc3381.poll.response": { "answers": [] },
        });
        assert!(
            PollEvent::parse(POLL_RESPONSE_EVENT_TYPE, None, "@a:hs", 1, &no_relation).is_err()
        );

        // Missing the answers array.
        let no_answers = json!({
            "m.relates_to": { "rel_type": "m.reference", "event_id": "$p:hs" },
            "org.matrix.msc3381.poll.response": {},
        });
        assert!(PollEvent::parse(POLL_RESPONSE_EVENT_TYPE, None, "@a:hs", 1, &no_answers).is_err());
    }

    #[test]
    fn rejects_missing_required_end_fields() {
        // Missing the required empty end marker.
        let no_marker = json!({
            "m.relates_to": { "rel_type": "m.reference", "event_id": "$p:hs" },
            "org.matrix.msc1767.text": "closed",
        });
        assert!(PollEvent::parse(POLL_END_EVENT_TYPE, None, "@a:hs", 1, &no_marker).is_err());

        // Missing the fallback text.
        let no_fallback = json!({
            "m.relates_to": { "rel_type": "m.reference", "event_id": "$p:hs" },
            "org.matrix.msc3381.poll.end": {},
        });
        assert!(PollEvent::parse(POLL_END_EVENT_TYPE, None, "@a:hs", 1, &no_fallback).is_err());
    }

    #[test]
    fn rejects_malformed_relation() {
        // Wrong relation type.
        let wrong_rel = json!({
            "m.relates_to": { "rel_type": "m.annotation", "event_id": "$p:hs" },
            "org.matrix.msc3381.poll.response": { "answers": ["a"] },
        });
        assert!(PollEvent::parse(POLL_RESPONSE_EVENT_TYPE, None, "@a:hs", 1, &wrong_rel).is_err());

        // Missing relation target.
        let no_target = json!({
            "m.relates_to": { "rel_type": "m.reference" },
            "org.matrix.msc3381.poll.response": { "answers": ["a"] },
        });
        assert!(PollEvent::parse(POLL_RESPONSE_EVENT_TYPE, None, "@a:hs", 1, &no_target).is_err());

        // Relation must be an object, not a string.
        let scalar_rel = json!({
            "m.relates_to": "m.reference",
            "org.matrix.msc3381.poll.end": {},
            "org.matrix.msc1767.text": "closed",
        });
        assert!(PollEvent::parse(POLL_END_EVENT_TYPE, None, "@a:hs", 1, &scalar_rel).is_err());
    }

    #[test]
    fn rejects_zero_max_selections() {
        let zero = json!({
            "org.matrix.msc1767.text": "fallback",
            "org.matrix.msc3381.poll.start": {
                "max_selections": 0,
                "question": { "org.matrix.msc1767.text": "Q" },
                "answers": [{ "id": "a", "org.matrix.msc1767.text": "A" }],
            },
        });
        assert!(matches!(
            PollEvent::parse(POLL_START_EVENT_TYPE, None, "@a:hs", 1, &zero),
            Err(PollEventError::Invalid(_))
        ));
    }

    #[test]
    fn applies_protocol_defaults_for_optional_start_fields() {
        let minimal = json!({
            "org.matrix.msc1767.text": "fallback",
            "org.matrix.msc3381.poll.start": {
                "question": { "org.matrix.msc1767.text": "Q" },
                "answers": [{ "id": "a", "org.matrix.msc1767.text": "A" }],
            },
        });
        let PollEvent::Start(start) =
            PollEvent::parse(POLL_START_EVENT_TYPE, None, "@a:hs", 1, &minimal)
                .unwrap()
                .unwrap()
        else {
            panic!("expected start");
        };
        // Absent kind defaults to undisclosed; absent max_selections defaults to 1.
        assert_eq!(start.content.poll.kind, PollKind::Undisclosed);
        assert!(!start.content.poll.kind.is_disclosed());
        assert_eq!(start.content.poll.max_selections, 1);
    }

    #[test]
    fn preserves_unknown_kind_and_treats_it_as_undisclosed() {
        let custom = json!({
            "org.matrix.msc1767.text": "fallback",
            "org.matrix.msc3381.poll.start": {
                "kind": "com.example.poll.secret",
                "question": { "org.matrix.msc1767.text": "Q" },
                "answers": [{ "id": "a", "org.matrix.msc1767.text": "A" }],
            },
        });
        let PollEvent::Start(start) =
            PollEvent::parse(POLL_START_EVENT_TYPE, None, "@a:hs", 1, &custom)
                .unwrap()
                .unwrap()
        else {
            panic!("expected start");
        };
        assert_eq!(
            start.content.poll.kind,
            PollKind::Other("com.example.poll.secret".to_string())
        );
        assert!(!start.content.poll.kind.is_disclosed());
    }

    #[test]
    fn tolerates_unknown_extra_fields() {
        let mut content = start_content();
        content["unknown.block"] = json!({ "x": 1 });
        content["org.matrix.msc3381.poll.start"]["extra"] = json!(true);
        content["org.matrix.msc3381.poll.start"]["answers"][0]["extra"] = json!("ok");
        assert!(
            PollEvent::parse(POLL_START_EVENT_TYPE, None, "@a:hs", 1, &content)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn from_matrix_event_parses_and_requires_sender() {
        let event = MatrixEvent {
            event_id: "$start:hs".to_string(),
            room_id: "!room:hs".to_string(),
            event_type: POLL_START_EVENT_TYPE.to_string(),
            state_key: None,
            sender: Some("@alice:hs".to_string()),
            origin_server_ts: 42,
            content: start_content(),
            redacted_by: None,
        };
        let parsed = PollEvent::from_matrix_event(&event).unwrap().unwrap();
        assert_eq!(parsed.event_type(), POLL_START_EVENT_TYPE);
        let PollEvent::Start(start) = parsed else {
            panic!("expected start");
        };
        assert_eq!(start.event_id.as_deref(), Some("$start:hs"));

        // A poll event whose envelope lost its sender is malformed, not ignored.
        let senderless = MatrixEvent {
            sender: None,
            ..event.clone()
        };
        assert!(matches!(
            PollEvent::from_matrix_event(&senderless),
            Err(PollEventError::MissingSender)
        ));

        // A non-poll event with no sender is simply not a poll event.
        let other = MatrixEvent {
            event_type: "m.room.message".to_string(),
            sender: None,
            ..event
        };
        assert!(PollEvent::from_matrix_event(&other).unwrap().is_none());
    }

    // ── Serialization ─────────────────────────────────────────────

    #[test]
    fn serializes_poll_start_to_pinned_wire_format() {
        let content = PollStartContent::new(
            "What should we order?\n1. Pizza\n2. Poutine",
            "What should we order?",
            vec![
                PollAnswer::new("pizza", "Pizza"),
                PollAnswer::new("poutine", "Poutine"),
            ],
            PollKind::Disclosed,
            1,
        );
        assert_eq!(serde_json::to_value(&content).unwrap(), start_content());
    }

    #[test]
    fn serializes_poll_response_to_pinned_wire_format() {
        let content = PollResponseContent::new("$poll:hs", vec!["poutine".to_string()]);
        assert_eq!(serde_json::to_value(&content).unwrap(), response_content());
    }

    #[test]
    fn serializes_poll_end_to_pinned_wire_format() {
        let content = PollEndContent::new("$poll:hs", "The poll has ended. Top answer: Pizza");
        assert_eq!(serde_json::to_value(&content).unwrap(), end_content());
    }

    #[test]
    fn end_marker_serializes_as_an_empty_object() {
        assert_eq!(serde_json::to_value(PollEndMarker {}).unwrap(), json!({}));
    }

    #[test]
    fn event_type_constants_match_pinned_revision() {
        assert_eq!(POLL_START_EVENT_TYPE, "org.matrix.msc3381.poll.start");
        assert_eq!(POLL_RESPONSE_EVENT_TYPE, "org.matrix.msc3381.poll.response");
        assert_eq!(POLL_END_EVENT_TYPE, "org.matrix.msc3381.poll.end");
        assert_eq!(POLL_KIND_DISCLOSED, "org.matrix.msc3381.poll.disclosed");
        assert_eq!(POLL_KIND_UNDISCLOSED, "org.matrix.msc3381.poll.undisclosed");
        assert_eq!(MSC1767_TEXT_KEY, "org.matrix.msc1767.text");
        assert_eq!(REFERENCE_RELATION_TYPE, "m.reference");
        assert!(is_poll_event_type(POLL_START_EVENT_TYPE));
        assert!(is_poll_event_type(POLL_RESPONSE_EVENT_TYPE));
        assert!(is_poll_event_type(POLL_END_EVENT_TYPE));
        assert!(!is_poll_event_type("m.poll.start"));
    }
}
