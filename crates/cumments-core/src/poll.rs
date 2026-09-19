//! Deterministic Poll projection and reduction (MSC3381).
//!
//! This module turns a poll's canonical Matrix facts into an effective Poll
//! state. It is pure: it performs no I/O and depends only on the facts handed
//! to it, so the same canonical Matrix event set always produces the same
//! projection regardless of sync, backfill, insertion or network ordering.
//!
//! It deliberately models only *facts* (a poll start, its response events and
//! its end events) and the reduction rules from the frozen Poll design:
//!
//! * answers are an ordered, opaque, case-sensitive sequence of client
//!   generated identifiers; their declared order is never sorted;
//! * vote selections are an unordered set: duplicates collapse, an empty
//!   selection is an explicit unvote, unknown answer identifiers and
//!   selections exceeding `max_selections` make the response invalid rather
//!   than being silently reinterpreted or truncated;
//! * only an author's most recent non-redacted response is effective, chosen
//!   by `(origin_server_ts, event_id)`;
//! * redaction reverts a voter to their previous surviving response;
//! * only the earliest valid, authorized, non-redacted `poll.end` is
//!   effective, chosen by `(origin_server_ts, event_id)`.
//!
//! End authorization is an explicit input ([`EndAuthorization`]): the reducer
//! never infers it from room power levels itself. The Cumments site-moderator
//! to Matrix redact-power mapping is intentionally not decided here.
//!
//! This module is the Matrix-derived projection layer; it never persists
//! anything and is not authoritative state. Durable operation identity,
//! signatures, proof-of-work and the public HTTP API live in later layers.

use crate::canonical::CanonicalJson;
use serde::{Deserialize, Serialize};

/// Minimum number of answers a poll must declare.
///
/// MSC3381 allows a Poll with a single option, so Cumments does too.
pub const MIN_POLL_ANSWERS: usize = 1;

/// Maximum number of answers a poll may declare.
///
/// MSC3381 defines the answer list as at most 20 options and receivers
/// truncate to 20 while processing. Cumments signs the authored definition, so
/// it rejects a longer list at the API boundary instead of truncating: a
/// silently shortened list would make the signed meaning and the emitted
/// Matrix Poll disagree.
pub const MAX_POLL_ANSWERS: usize = 20;

/// Signature protocol domain separator (frozen design §9.1).
pub const SIGNATURE_DOMAIN: &str = "host.curious.cumments.signature";

/// Signature protocol version (frozen design §9.1).
pub const SIGNATURE_VERSION: &str = "1";

/// Canonical semantic-operation schema version (frozen design §9.3).
pub const SEMANTIC_SCHEMA_VERSION: i64 = 1;

/// A poll's kind in the signed semantic operation (frozen design §9.5.1).
///
/// Distinct from the Matrix wire `kind` (`org.matrix.msc3381.poll.*`): the
/// semantic operation binds the bare `"disclosed"` / `"undisclosed"` value,
/// keeping the client protocol independent of the Matrix wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PollSemanticKind {
    Disclosed,
    Undisclosed,
}

impl PollSemanticKind {
    /// The value used in the canonical semantic operation.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Disclosed => "disclosed",
            Self::Undisclosed => "undisclosed",
        }
    }
}

/// One caller-authored answer in the signed semantic operation.
///
/// `id` is an opaque, case-sensitive, caller-generated token; `text` is its
/// presentation label. Declared order is preserved exactly and never sorted.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PollSemanticAnswer {
    pub id: String,
    pub text: String,
}

impl PollSemanticAnswer {
    pub fn new(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
        }
    }
}

/// Why a Poll definition is not semantically valid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PollSemanticError {
    #[error("question must not be empty")]
    EmptyQuestion,
    #[error("a poll must declare between {MIN_POLL_ANSWERS} and {MAX_POLL_ANSWERS} answers")]
    AnswerCount,
    #[error("answer id {0:?} is duplicated")]
    DuplicateAnswerId(String),
    #[error("answer text must not be empty")]
    EmptyAnswerText,
    #[error("max_selections must be at least 1")]
    MaxSelections,
    #[error("max_selections {0} cannot be represented in a signed poll")]
    MaxSelectionsNotRepresentable(u64),
}

/// Validate a Poll definition against the semantic contract.
///
/// The contract is the application's own, and deliberately narrow:
///
/// * the question is non-empty application text with no maximum length;
/// * a poll declares 1 to [`MAX_POLL_ANSWERS`] answers, in declared order;
/// * an answer id is an opaque string, unique within this poll — the empty
///   string is a legal id, and there is no character-set or length rule;
/// * answer text is non-empty application text with no maximum length;
/// * `max_selections` is at least 1 and may exceed the number of answers.
///
/// Only the answer count is capped, and by the Matrix Poll limit rather than by
/// an application preference. Answer order is preserved and answer ids are
/// compared byte-for-byte (case-sensitive).
pub fn validate_poll_semantic_definition(
    question: &str,
    answers: &[PollSemanticAnswer],
    max_selections: u64,
) -> Result<(), PollSemanticError> {
    if question.trim().is_empty() {
        return Err(PollSemanticError::EmptyQuestion);
    }
    if !(MIN_POLL_ANSWERS..=MAX_POLL_ANSWERS).contains(&answers.len()) {
        return Err(PollSemanticError::AnswerCount);
    }
    for (index, answer) in answers.iter().enumerate() {
        if answers[..index].iter().any(|prior| prior.id == answer.id) {
            return Err(PollSemanticError::DuplicateAnswerId(answer.id.clone()));
        }
        if answer.text.trim().is_empty() {
            return Err(PollSemanticError::EmptyAnswerText);
        }
    }
    if max_selections < 1 {
        return Err(PollSemanticError::MaxSelections);
    }
    // `max_selections` reaches the signed operation as a canonical integer, so
    // the only upper bound is what Matrix Canonical JSON can represent.
    if CanonicalJson::int_from_u64(max_selections).is_none() {
        return Err(PollSemanticError::MaxSelectionsNotRepresentable(
            max_selections,
        ));
    }
    Ok(())
}

/// Build the canonical `POLL` semantic operation (frozen design §9.5.1):
///
/// ```text
/// ["POLL",
///  [site_id, page_slug, reply_to, thread_root],
///  [question, [[answer_id, answer_text], ...], kind, max_selections],
///  1]
/// ```
///
/// `reply_to` and `thread_root` are independent and encoded as JSON `null`
/// when absent. This value is the common input to fingerprinting, the
/// signature envelope and Matrix provenance.
///
/// Returns `None` when `max_selections` cannot be represented in the signed
/// representation, which [`validate_poll_semantic_definition`] rejects before
/// any validated definition reaches here.
#[allow(clippy::too_many_arguments)]
pub fn poll_semantic_operation(
    site_id: &str,
    page_slug: &str,
    reply_to: Option<&str>,
    thread_root: Option<&str>,
    question: &str,
    answers: &[PollSemanticAnswer],
    kind: PollSemanticKind,
    max_selections: u64,
) -> Option<CanonicalJson> {
    let max_selections = CanonicalJson::int_from_u64(max_selections)?;
    let encoded_answers: Vec<CanonicalJson> = answers
        .iter()
        .map(|answer| {
            CanonicalJson::array(vec![
                CanonicalJson::string(answer.id.clone()),
                CanonicalJson::string(answer.text.clone()),
            ])
        })
        .collect();
    Some(CanonicalJson::array(vec![
        CanonicalJson::string("POLL"),
        CanonicalJson::array(vec![
            CanonicalJson::string(site_id),
            CanonicalJson::string(page_slug),
            CanonicalJson::nullable_string(reply_to),
            CanonicalJson::nullable_string(thread_root),
        ]),
        CanonicalJson::array(vec![
            CanonicalJson::string(question),
            CanonicalJson::array(encoded_answers),
            CanonicalJson::string(kind.as_str()),
            max_selections,
        ]),
        CanonicalJson::int(SEMANTIC_SCHEMA_VERSION),
    ]))
}

/// The semantic fingerprint: `SHA-256` of the canonical semantic operation's
/// UTF-8 bytes, hex-encoded. The fingerprint input is exactly the canonical
/// semantic operation, never transport JSON.
pub fn poll_semantic_fingerprint(operation: &CanonicalJson) -> String {
    crate::site_auth::sha256_hex(&operation.to_canonical_bytes())
}

/// Build the signed envelope (frozen design §9.4):
///
/// ```text
/// ["host.curious.cumments.signature", "1", <operation>, operation_id, challenge]
/// ```
///
/// The author's public key is deliberately not part of the envelope.
pub fn poll_signature_envelope(
    operation: &CanonicalJson,
    operation_id: &str,
    challenge: &str,
) -> CanonicalJson {
    CanonicalJson::array(vec![
        CanonicalJson::string(SIGNATURE_DOMAIN),
        CanonicalJson::string(SIGNATURE_VERSION),
        operation.clone(),
        CanonicalJson::string(operation_id),
        CanonicalJson::string(challenge),
    ])
}

/// Verify a `POLL` signature over the canonical envelope.
pub fn verify_poll_signature(
    public_key_b64: &str,
    operation: &CanonicalJson,
    operation_id: &str,
    challenge: &str,
    signature_b64: &str,
) -> bool {
    let message = poll_signature_envelope(operation, operation_id, challenge).to_canonical_string();
    crate::identity::verify_signature(public_key_b64, &message, signature_b64)
}

/// The semantic Poll meaning actually encoded by a `poll.start` wire event.
///
/// A visitor Poll event carries two independent things: the signed canonical
/// operation in its provenance block, and the Matrix wire content. Trusting the
/// event requires establishing that they denote the same Poll, per the frozen
/// design's invariant:
///
/// ```text
/// provenance.content == canonical semantic operation
///                    == fingerprint input
///                    == signed semantic value
///                    == semantic meaning encoded by poll.start
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollWireSemantics {
    pub site_id: String,
    pub page_slug: String,
    pub reply_to: Option<String>,
    pub thread_root: Option<String>,
    pub question: String,
    /// Answers in declared wire order; never sorted.
    pub answers: Vec<PollSemanticAnswer>,
    pub kind: PollSemanticKind,
    pub max_selections: u64,
}

impl PollWireSemantics {
    /// The canonical semantic operation this wire content denotes, or `None`
    /// when the wire content cannot denote one (its `max_selections` is outside
    /// the signed representation, so no valid signature can cover it).
    pub fn to_semantic_operation(&self) -> Option<CanonicalJson> {
        poll_semantic_operation(
            &self.site_id,
            &self.page_slug,
            self.reply_to.as_deref(),
            self.thread_root.as_deref(),
            &self.question,
            &self.answers,
            self.kind,
            self.max_selections,
        )
    }
}

/// Verify a visitor `poll.start` proof against the actual Poll wire content.
///
/// Succeeds only when the signature verifies over `signed_operation` **and**
/// that signed operation is byte-for-byte the canonical operation the wire
/// content denotes. The provenance block is never trusted as an independent
/// declaration of the Poll's meaning.
#[allow(clippy::too_many_arguments)] // mirrors the wire facts and proof
pub fn verify_poll_start_proof(
    public_key_b64: &str,
    signed_operation: &CanonicalJson,
    operation_id: &str,
    challenge: &str,
    signature_b64: &str,
    wire: &PollWireSemantics,
) -> bool {
    wire.to_semantic_operation()
        .is_some_and(|operation| operation == *signed_operation)
        && verify_poll_signature(
            public_key_b64,
            signed_operation,
            operation_id,
            challenge,
            signature_b64,
        )
}

/// Why raw vote selections are not a valid canonical selection set.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VoteSelectionError {
    #[error("at most {max} selection(s) are allowed")]
    TooMany { max: u64 },
    #[error("answer id {0:?} is not an option of the target poll")]
    UnknownId(String),
}

/// Normalize raw vote selections into the canonical selection set.
///
/// Selections are an unordered set: deduplicate exactly, sort byte-wise, enforce
/// `max_selections`, then require every id to exist on the target poll. Answer
/// ids are opaque strings, so there is no syntax stage — the empty string is a
/// legal id when the target poll declares it. Order is only canonicalized for
/// the signed representation; it never affects the poll's declared answer
/// order. An empty input is a valid explicit unvote.
pub fn normalize_vote_selections(
    raw: &[String],
    max_selections: u64,
    known_option_ids: &[String],
) -> Result<Vec<String>, VoteSelectionError> {
    let mut canonical: Vec<String> = Vec::with_capacity(raw.len());
    for id in raw {
        if !canonical.contains(id) {
            canonical.push(id.clone());
        }
    }
    canonical.sort();
    if canonical.len() as u64 > max_selections {
        return Err(VoteSelectionError::TooMany {
            max: max_selections,
        });
    }
    for id in &canonical {
        if !known_option_ids.iter().any(|known| known == id) {
            return Err(VoteSelectionError::UnknownId(id.clone()));
        }
    }
    Ok(canonical)
}

/// Build the canonical `VOTE` semantic operation (frozen design §9.5.2):
///
/// ```text
/// ["VOTE", [site_id, page_slug, poll_event_id], [canonical_option_ids...], 1]
/// ```
///
/// The payload is already normalized (deduplicated and byte-wise sorted), so
/// the same selection set always produces the same operation regardless of the
/// order the client supplied.
pub fn vote_semantic_operation(
    site_id: &str,
    page_slug: &str,
    poll_event_id: &str,
    canonical_option_ids: &[String],
) -> CanonicalJson {
    CanonicalJson::array(vec![
        CanonicalJson::string("VOTE"),
        CanonicalJson::array(vec![
            CanonicalJson::string(site_id),
            CanonicalJson::string(page_slug),
            CanonicalJson::string(poll_event_id),
        ]),
        CanonicalJson::array(
            canonical_option_ids
                .iter()
                .map(|id| CanonicalJson::string(id.clone()))
                .collect(),
        ),
        CanonicalJson::int(SEMANTIC_SCHEMA_VERSION),
    ])
}

/// The semantic VOTE meaning actually encoded by a `poll.response` wire event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoteWireSemantics {
    pub site_id: String,
    pub page_slug: String,
    pub poll_event_id: String,
    /// Canonical (deduplicated, byte-wise sorted) selections.
    pub option_ids: Vec<String>,
}

impl VoteWireSemantics {
    /// The canonical semantic operation this wire content denotes.
    pub fn to_semantic_operation(&self) -> CanonicalJson {
        vote_semantic_operation(
            &self.site_id,
            &self.page_slug,
            &self.poll_event_id,
            &self.option_ids,
        )
    }
}

/// Verify a visitor `poll.response` proof against the actual Vote wire content.
///
/// Succeeds only when the signature verifies over `signed_operation` and that
/// signed operation is byte-for-byte the canonical VOTE operation the wire
/// content denotes.
pub fn verify_vote_proof(
    public_key_b64: &str,
    signed_operation: &CanonicalJson,
    operation_id: &str,
    challenge: &str,
    signature_b64: &str,
    wire: &VoteWireSemantics,
) -> bool {
    wire.to_semantic_operation() == *signed_operation
        && verify_poll_signature(
            public_key_b64,
            signed_operation,
            operation_id,
            challenge,
            signature_b64,
        )
}

/// Build the canonical `END_POLL` semantic operation (frozen design §9.5.3):
///
/// ```text
/// ["END_POLL", [site_id, page_slug, poll_event_id], [], 1]
/// ```
///
/// The payload is intentionally an empty array.
pub fn end_poll_semantic_operation(
    site_id: &str,
    page_slug: &str,
    poll_event_id: &str,
) -> CanonicalJson {
    CanonicalJson::array(vec![
        CanonicalJson::string("END_POLL"),
        CanonicalJson::array(vec![
            CanonicalJson::string(site_id),
            CanonicalJson::string(page_slug),
            CanonicalJson::string(poll_event_id),
        ]),
        CanonicalJson::array(vec![]),
        CanonicalJson::int(SEMANTIC_SCHEMA_VERSION),
    ])
}

/// The semantic END_POLL meaning actually encoded by a `poll.end` wire event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndPollWireSemantics {
    pub site_id: String,
    pub page_slug: String,
    pub poll_event_id: String,
}

impl EndPollWireSemantics {
    /// The canonical semantic operation this wire content denotes.
    pub fn to_semantic_operation(&self) -> CanonicalJson {
        end_poll_semantic_operation(&self.site_id, &self.page_slug, &self.poll_event_id)
    }
}

/// Verify a visitor `poll.end` proof against the actual End wire content.
///
/// Succeeds only when the signature verifies over `signed_operation` and that
/// signed operation is byte-for-byte the canonical END_POLL operation the wire
/// content denotes.
#[allow(clippy::too_many_arguments)]
pub fn verify_end_poll_proof(
    public_key_b64: &str,
    signed_operation: &CanonicalJson,
    operation_id: &str,
    challenge: &str,
    signature_b64: &str,
    wire: &EndPollWireSemantics,
) -> bool {
    wire.to_semantic_operation() == *signed_operation
        && verify_poll_signature(
            public_key_b64,
            signed_operation,
            operation_id,
            challenge,
            signature_b64,
        )
}

/// One declared answer option, in presentation order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollAnswerFact {
    pub id: String,
    pub text: String,
}

impl PollAnswerFact {
    pub fn new(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
        }
    }
}

/// A poll start fact: the canonical `org.matrix.msc3381.poll.start` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollStartFact {
    pub event_id: String,
    pub sender: String,
    pub origin_server_ts: i64,
    pub question: String,
    /// Answers in declared presentation order; never sorted.
    pub answers: Vec<PollAnswerFact>,
    pub max_selections: u64,
    /// Whether results are visible while the poll is open.
    pub disclosed: bool,
    /// Matrix event ID of the parent comment, when the enclosing Cumments
    /// event carried a reply relation.
    pub reply_to: Option<String>,
    /// Matrix event ID of the thread root, when the enclosing Cumments event
    /// carried a thread relation.
    pub thread_root: Option<String>,
}

/// Why a poll start is not a valid poll definition.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PollStartError {
    #[error("poll has no answers")]
    NoAnswers,
    #[error("poll answer id {0:?} is duplicated")]
    DuplicateAnswerId(String),
    #[error("poll max_selections must be at least 1")]
    ZeroMaxSelections,
}

impl PollStartFact {
    /// Validate the poll definition: at least one answer, at least one
    /// selection, and answer ids unique within the poll. Answer ids are opaque
    /// (any string, including the empty one) and the declared order is
    /// preserved; ids are compared byte-for-byte (case-sensitive).
    pub fn validate(&self) -> Result<(), PollStartError> {
        if self.max_selections < 1 {
            return Err(PollStartError::ZeroMaxSelections);
        }
        if self.answers.is_empty() {
            return Err(PollStartError::NoAnswers);
        }
        for (index, answer) in self.answers.iter().enumerate() {
            if self.answers[..index]
                .iter()
                .any(|prior| prior.id == answer.id)
            {
                return Err(PollStartError::DuplicateAnswerId(answer.id.clone()));
            }
        }
        Ok(())
    }

    /// The declared answer identifiers, in order.
    pub fn answer_ids(&self) -> Vec<String> {
        self.answers
            .iter()
            .map(|answer| answer.id.clone())
            .collect()
    }
}

/// A poll response fact: one canonical `org.matrix.msc3381.poll.response`
/// event. `selections` are the raw identifiers as delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollResponseFact {
    pub event_id: String,
    pub sender: String,
    pub origin_server_ts: i64,
    pub selections: Vec<String>,
    /// Redacted responses keep their relation but not their content, so they
    /// are ignored by the reduction (reverting the voter's prior response).
    pub redacted: bool,
}

impl PollResponseFact {
    pub fn new(
        event_id: impl Into<String>,
        sender: impl Into<String>,
        origin_server_ts: i64,
        selections: Vec<String>,
    ) -> Self {
        Self {
            event_id: event_id.into(),
            sender: sender.into(),
            origin_server_ts,
            selections,
            redacted: false,
        }
    }
}

/// Authorization outcome for a `poll.end` event.
///
/// MSC3381 makes an end valid only when sent by the poll's original creator
/// or by a user who may redact other users' messages. The reducer takes this
/// decision as input so the unresolved Cumments-site-moderator to Matrix
/// redact-power mapping is never invented here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndAuthorization {
    /// The end was sent by the poll's original creator.
    Creator,
    /// The end's sender holds room power to redact other users' messages.
    RedactPower,
    /// Authorization could not be established; the end is not effective.
    Unauthorized,
}

impl EndAuthorization {
    /// Whether an end with this authorization may close the poll.
    pub fn is_authorized(&self) -> bool {
        matches!(self, Self::Creator | Self::RedactPower)
    }

    /// Derive end authorization from DAG-consistent Poll facts.
    ///
    /// Only the creator check is decidable from the canonical event set: the
    /// poll start's sender *is* the poll creator, so an end sent by that same
    /// sender is authorized by the facts themselves.
    ///
    /// MSC3381 also accepts senders that may redact other users' messages, but
    /// that depends on the room state at the event's position in the Matrix
    /// DAG. Cumments has no historical room-state authorization layer yet, so
    /// any other sender is treated as [`EndAuthorization::Unauthorized`] rather
    /// than guessed from currently visible room power levels. This keeps
    /// projection correctness independent of when local processing happened to
    /// read room state; the later authorization layer revisits this explicit
    /// boundary.
    pub fn from_facts(poll_sender: &str, end_sender: &str) -> Self {
        if poll_sender == end_sender {
            Self::Creator
        } else {
            Self::Unauthorized
        }
    }
}

/// A poll end fact: one canonical `org.matrix.msc3381.poll.end` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollEndFact {
    pub event_id: String,
    pub sender: String,
    pub origin_server_ts: i64,
    pub authorization: EndAuthorization,
    /// Redacted ends are ignored, exactly like redacted responses.
    pub redacted: bool,
}

/// Whether a poll is open or has been effectively closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PollStatus {
    Open,
    Ended,
}

/// The effective end of a poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollEndProjection {
    pub event_id: String,
    pub sender: String,
    pub origin_server_ts: i64,
}

/// An author's effective response after reduction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollVoteProjection {
    pub sender: String,
    pub event_id: String,
    pub origin_server_ts: i64,
    /// Effective selections: deduplicated and sorted byte-wise. Empty for an
    /// explicit unvote or a spoiled response.
    pub selections: Vec<String>,
    /// Whether the response was invalid (unknown identifier or too many
    /// selections) rather than a valid unvote.
    pub spoiled: bool,
}

/// One declared answer's vote count. Zero counts are included so the read
/// model can render every option deterministically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollAnswerTally {
    pub answer_id: String,
    pub count: u64,
}

/// The deterministic, derived state of one poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollProjection {
    pub poll_event_id: String,
    pub sender: String,
    pub created_at: i64,
    pub question: String,
    /// Answers in declared order, as published by the poll start.
    pub answers: Vec<PollAnswerFact>,
    pub max_selections: u64,
    pub disclosed: bool,
    pub reply_to: Option<String>,
    pub thread_root: Option<String>,
    pub status: PollStatus,
    /// The first valid, authorized end, if any.
    pub end: Option<PollEndProjection>,
    /// One entry per responding author, ordered by sender.
    pub votes: Vec<PollVoteProjection>,
    /// Counts in declared answer order, including zero counts.
    pub tallies: Vec<PollAnswerTally>,
    /// Number of authors with at least one effective selection. Unvotes and
    /// spoiled responses do not count.
    pub total_votes: u64,
}

impl PollProjection {
    /// The declared answer ids, in presentation order.
    pub fn answer_ids(&self) -> Vec<&str> {
        self.answers
            .iter()
            .map(|answer| answer.id.as_str())
            .collect()
    }

    /// The declared answer id at `index`, if any.
    pub fn answer_id(&self, index: usize) -> Option<&str> {
        self.answers.get(index).map(|answer| answer.id.as_str())
    }

    /// Vote count for one declared answer id.
    pub fn count_for(&self, answer_id: &str) -> u64 {
        self.tallies
            .iter()
            .find(|tally| tally.answer_id == answer_id)
            .map(|tally| tally.count)
            .unwrap_or(0)
    }

    /// One author's effective selections, if they have a surviving response.
    pub fn vote_of(&self, sender: &str) -> Option<&PollVoteProjection> {
        self.votes.iter().find(|vote| vote.sender == sender)
    }
}

/// Reduce a poll's canonical Matrix facts into its effective state.
///
/// Returns [`PollStartError`] when the poll definition itself is invalid; no
/// projection exists for an invalid poll. Malformed *individual* responses or
/// ends never corrupt the projection: they are ignored or spoil only their
/// own author's vote.
pub fn reduce_poll(
    start: &PollStartFact,
    responses: &[PollResponseFact],
    ends: &[PollEndFact],
) -> Result<PollProjection, PollStartError> {
    start.validate()?;
    let known: Vec<&str> = start.answers.iter().map(|a| a.id.as_str()).collect();

    // The effective end is the earliest valid, authorized, non-redacted end by
    // (origin_server_ts, event_id). Later valid ends are non-effective.
    let end = ends
        .iter()
        .filter(|end| !end.redacted && end.authorization.is_authorized())
        .min_by(|a, b| {
            (a.origin_server_ts, a.event_id.as_str())
                .cmp(&(b.origin_server_ts, b.event_id.as_str()))
        })
        .map(|end| PollEndProjection {
            event_id: end.event_id.clone(),
            sender: end.sender.clone(),
            origin_server_ts: end.origin_server_ts,
        });

    // Only the author's most recent non-redacted response is effective.
    // Redaction reverts the author to their previous surviving response.
    let mut latest: std::collections::HashMap<&str, &PollResponseFact> =
        std::collections::HashMap::new();
    for response in responses.iter().filter(|response| !response.redacted) {
        // Responses that arrived after the poll closed are ignored, as if they
        // never happened. Votes on or before the closing timestamp count.
        if end
            .as_ref()
            .is_some_and(|end| response.origin_server_ts > end.origin_server_ts)
        {
            continue;
        }
        match latest.get(response.sender.as_str()) {
            Some(current)
                if (current.origin_server_ts, current.event_id.as_str())
                    >= (response.origin_server_ts, response.event_id.as_str()) => {}
            _ => {
                latest.insert(response.sender.as_str(), response);
            }
        }
    }

    let mut votes: Vec<PollVoteProjection> = latest
        .into_values()
        .map(|response| normalize_response(response, &known, start.max_selections))
        .collect();
    votes.sort_by(|a, b| a.sender.cmp(&b.sender));

    let mut tallies: Vec<PollAnswerTally> = start
        .answers
        .iter()
        .map(|answer| PollAnswerTally {
            answer_id: answer.id.clone(),
            count: 0,
        })
        .collect();
    for vote in &votes {
        for selection in &vote.selections {
            if let Some(tally) = tallies
                .iter_mut()
                .find(|tally| &tally.answer_id == selection)
            {
                tally.count += 1;
            }
        }
    }
    let total_votes = votes
        .iter()
        .filter(|vote| !vote.selections.is_empty())
        .count() as u64;

    Ok(PollProjection {
        poll_event_id: start.event_id.clone(),
        sender: start.sender.clone(),
        created_at: start.origin_server_ts,
        question: start.question.clone(),
        answers: start.answers.clone(),
        max_selections: start.max_selections,
        disclosed: start.disclosed,
        reply_to: start.reply_to.clone(),
        thread_root: start.thread_root.clone(),
        status: if end.is_some() {
            PollStatus::Ended
        } else {
            PollStatus::Open
        },
        end,
        votes,
        tallies,
        total_votes,
    })
}

/// Normalize one author's effective response.
///
/// A response is invalid if it selects an unknown answer or more selections
/// than the poll allows; invalid responses are spoiled (no effective
/// selection) rather than reinterpreted or truncated. An empty selection is a
/// valid explicit unvote. Valid selections are deduplicated and sorted
/// byte-wise so the representation is order-independent.
fn normalize_response(
    response: &PollResponseFact,
    known: &[&str],
    max_selections: u64,
) -> PollVoteProjection {
    let mut selections: Vec<String> = Vec::new();
    let mut unknown = false;
    for selection in &response.selections {
        if !known.contains(&selection.as_str()) {
            unknown = true;
            break;
        }
        if !selections.contains(selection) {
            selections.push(selection.clone());
        }
    }
    if unknown {
        return spoiled_vote(response);
    }
    if selections.len() as u64 > max_selections {
        return spoiled_vote(response);
    }
    selections.sort();
    PollVoteProjection {
        sender: response.sender.clone(),
        event_id: response.event_id.clone(),
        origin_server_ts: response.origin_server_ts,
        selections,
        spoiled: false,
    }
}

fn spoiled_vote(response: &PollResponseFact) -> PollVoteProjection {
    PollVoteProjection {
        sender: response.sender.clone(),
        event_id: response.event_id.clone(),
        origin_server_ts: response.origin_server_ts,
        selections: Vec::new(),
        spoiled: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(max_selections: u64, disclosed: bool) -> PollStartFact {
        PollStartFact {
            event_id: "$poll:hs".to_string(),
            sender: "@alice:hs".to_string(),
            origin_server_ts: 100,
            question: "Which meeting time works best?".to_string(),
            answers: vec![
                PollAnswerFact::new("slot-10am", "10:00 AM UTC"),
                PollAnswerFact::new("slot-2pm", "2:00 PM UTC"),
                PollAnswerFact::new("slot-6pm", "6:00 PM UTC"),
            ],
            max_selections,
            disclosed,
            reply_to: None,
            thread_root: None,
        }
    }

    fn response(event_id: &str, sender: &str, ts: i64, selections: &[&str]) -> PollResponseFact {
        PollResponseFact::new(
            event_id,
            sender,
            ts,
            selections.iter().map(|s| s.to_string()).collect(),
        )
    }

    fn end(event_id: &str, sender: &str, ts: i64, authorization: EndAuthorization) -> PollEndFact {
        PollEndFact {
            event_id: event_id.to_string(),
            sender: sender.to_string(),
            origin_server_ts: ts,
            authorization,
            redacted: false,
        }
    }

    // ── Poll creation ─────────────────────────────────────────────

    #[test]
    fn valid_start_projects_answers_in_declared_order() {
        let projection = reduce_poll(&start(1, true), &[], &[]).expect("valid start");
        assert_eq!(projection.poll_event_id, "$poll:hs");
        assert_eq!(projection.sender, "@alice:hs");
        assert_eq!(projection.created_at, 100);
        assert_eq!(projection.question, "Which meeting time works best?");
        assert_eq!(projection.max_selections, 1);
        assert!(projection.disclosed);
        assert_eq!(projection.status, PollStatus::Open);
        assert!(projection.end.is_none());
        // Declared order is preserved, never sorted.
        assert_eq!(
            projection.answer_ids(),
            vec!["slot-10am", "slot-2pm", "slot-6pm"]
        );
        assert_eq!(projection.answer_id(0), Some("slot-10am"));
        // Zero counts are included for every declared answer.
        assert_eq!(
            projection.tallies,
            vec![
                PollAnswerTally {
                    answer_id: "slot-10am".into(),
                    count: 0
                },
                PollAnswerTally {
                    answer_id: "slot-2pm".into(),
                    count: 0
                },
                PollAnswerTally {
                    answer_id: "slot-6pm".into(),
                    count: 0
                },
            ]
        );
        assert_eq!(projection.total_votes, 0);
    }

    #[test]
    fn undisclosed_kind_is_preserved() {
        let projection = reduce_poll(&start(1, false), &[], &[]).expect("valid start");
        assert!(!projection.disclosed);
    }

    #[test]
    fn start_without_answers_is_rejected() {
        let mut fact = start(1, true);
        fact.answers.clear();
        assert_eq!(reduce_poll(&fact, &[], &[]), Err(PollStartError::NoAnswers));
    }

    #[test]
    fn duplicate_answer_ids_are_rejected() {
        let mut fact = start(1, true);
        fact.answers = vec![
            PollAnswerFact::new("a", "A"),
            PollAnswerFact::new("b", "B"),
            PollAnswerFact::new("a", "A again"),
        ];
        assert_eq!(
            reduce_poll(&fact, &[], &[]),
            Err(PollStartError::DuplicateAnswerId("a".to_string()))
        );
    }

    #[test]
    fn answer_id_case_sensitivity_is_exact() {
        // "A" and "a" are distinct identifiers, not duplicates.
        let mut fact = start(1, true);
        fact.answers = vec![
            PollAnswerFact::new("A", "Upper"),
            PollAnswerFact::new("a", "Lower"),
        ];
        assert!(reduce_poll(&fact, &[], &[]).is_ok());
    }

    #[test]
    fn answer_ids_are_opaque_strings() {
        // Any string is a legal id: no character set, no length rule, and the
        // empty string is allowed.
        for id in [
            "",
            "with space",
            "emoji-🙂",
            "tab\tid",
            "slash/id",
            "中文字",
            "punct!\"#$%&'()*+,:;<=>?@[\\]^`{|}~",
            &"x".repeat(65),
            &"x".repeat(4096),
        ] {
            let mut fact = start(1, true);
            fact.answers = vec![PollAnswerFact::new(id, "X")];
            assert!(
                reduce_poll(&fact, &[], &[]).is_ok(),
                "{id:?} must be an accepted answer id"
            );
        }
    }

    #[test]
    fn one_answer_is_a_valid_poll() {
        let mut fact = start(1, true);
        fact.answers = vec![PollAnswerFact::new("only", "Only")];
        let projection = reduce_poll(&fact, &[], &[]).expect("single-answer poll");
        assert_eq!(projection.answers.len(), 1);
    }

    #[test]
    fn zero_max_selections_is_rejected() {
        assert_eq!(
            reduce_poll(&start(0, true), &[], &[]),
            Err(PollStartError::ZeroMaxSelections)
        );
    }

    #[test]
    fn max_selections_may_exceed_the_answer_count() {
        let mut fact = start(5, true);
        fact.answers = vec![PollAnswerFact::new("a", "A"), PollAnswerFact::new("b", "B")];
        let projection = reduce_poll(&fact, &[], &[]).expect("multi-select poll");
        assert_eq!(projection.max_selections, 5);
    }

    // ── Voting ────────────────────────────────────────────────────

    #[test]
    fn one_valid_response_is_counted() {
        let projection = reduce_poll(
            &start(1, true),
            &[response("$v1:hs", "@bob:hs", 200, &["slot-2pm"])],
            &[],
        )
        .unwrap();
        assert_eq!(projection.count_for("slot-2pm"), 1);
        assert_eq!(projection.count_for("slot-10am"), 0);
        assert_eq!(projection.total_votes, 1);
        let vote = projection.vote_of("@bob:hs").expect("bob voted");
        assert_eq!(vote.selections, vec!["slot-2pm"]);
        assert!(!vote.spoiled);
    }

    #[test]
    fn later_response_supersedes_earlier_for_same_author() {
        let projection = reduce_poll(
            &start(1, true),
            &[
                response("$v1:hs", "@bob:hs", 200, &["slot-10am"]),
                response("$v2:hs", "@bob:hs", 300, &["slot-6pm"]),
            ],
            &[],
        )
        .unwrap();
        assert_eq!(projection.count_for("slot-10am"), 0);
        assert_eq!(projection.count_for("slot-6pm"), 1);
        assert_eq!(projection.total_votes, 1);
        assert_eq!(projection.vote_of("@bob:hs").unwrap().event_id, "$v2:hs");
    }

    #[test]
    fn equal_timestamps_break_ties_by_event_id_ascending() {
        // The greater event id is the later fact, matching the projector's
        // (origin_server_ts, event_id) ordering.
        let projection = reduce_poll(
            &start(1, true),
            &[
                response("$a:hs", "@bob:hs", 200, &["slot-10am"]),
                response("$b:hs", "@bob:hs", 200, &["slot-6pm"]),
            ],
            &[],
        )
        .unwrap();
        assert_eq!(projection.vote_of("@bob:hs").unwrap().event_id, "$b:hs");
        assert_eq!(projection.count_for("slot-6pm"), 1);
    }

    #[test]
    fn duplicate_selections_collapse_to_one() {
        let projection = reduce_poll(
            &start(2, true),
            &[response(
                "$v1:hs",
                "@bob:hs",
                200,
                &["slot-2pm", "slot-2pm", "slot-10am"],
            )],
            &[],
        )
        .unwrap();
        let vote = projection.vote_of("@bob:hs").unwrap();
        // Set semantics: deduplicated, and represented in sorted order.
        assert_eq!(vote.selections, vec!["slot-10am", "slot-2pm"]);
        assert!(!vote.spoiled);
        assert_eq!(projection.count_for("slot-10am"), 1);
        assert_eq!(projection.count_for("slot-2pm"), 1);
    }

    #[test]
    fn empty_response_is_an_explicit_unvote() {
        let projection = reduce_poll(
            &start(1, true),
            &[
                response("$v1:hs", "@bob:hs", 200, &["slot-10am"]),
                response("$v2:hs", "@bob:hs", 300, &[]),
            ],
            &[],
        )
        .unwrap();
        let vote = projection.vote_of("@bob:hs").unwrap();
        assert!(vote.selections.is_empty());
        assert!(!vote.spoiled, "an unvote is valid, not spoiled");
        assert_eq!(projection.total_votes, 0);
        assert_eq!(projection.count_for("slot-10am"), 0);
    }

    #[test]
    fn unknown_answer_does_not_become_a_valid_selection() {
        let projection = reduce_poll(
            &start(2, true),
            &[response("$v1:hs", "@bob:hs", 200, &["slot-10am", "nope"])],
            &[],
        )
        .unwrap();
        let vote = projection.vote_of("@bob:hs").unwrap();
        assert!(vote.spoiled, "unknown answers spoil the response");
        assert!(vote.selections.is_empty());
        assert_eq!(projection.total_votes, 0);
        assert_eq!(projection.count_for("slot-10am"), 0);
    }

    #[test]
    fn unknown_answer_spoils_even_a_later_response() {
        // The most recent response wins even when invalid; an earlier valid
        // response is not resurrected by a newer spoiled one.
        let projection = reduce_poll(
            &start(2, true),
            &[
                response("$v1:hs", "@bob:hs", 200, &["slot-10am"]),
                response("$v2:hs", "@bob:hs", 300, &["nope"]),
            ],
            &[],
        )
        .unwrap();
        assert_eq!(projection.count_for("slot-10am"), 0);
        assert_eq!(projection.total_votes, 0);
        assert!(projection.vote_of("@bob:hs").unwrap().spoiled);
    }

    #[test]
    fn excessive_selections_are_not_silently_truncated() {
        let projection = reduce_poll(
            &start(1, true),
            &[response(
                "$v1:hs",
                "@bob:hs",
                200,
                &["slot-10am", "slot-2pm"],
            )],
            &[],
        )
        .unwrap();
        let vote = projection.vote_of("@bob:hs").unwrap();
        assert!(
            vote.spoiled,
            "selections over max_selections must not be truncated to the first one"
        );
        assert!(vote.selections.is_empty());
        assert_eq!(projection.total_votes, 0);
    }

    #[test]
    fn different_authors_vote_independently() {
        let projection = reduce_poll(
            &start(2, true),
            &[
                response("$a:hs", "@alice:hs", 200, &["slot-10am"]),
                response("$b:hs", "@bob:hs", 201, &["slot-2pm"]),
                response("$c:hs", "@carol:hs", 202, &["slot-2pm"]),
            ],
            &[],
        )
        .unwrap();
        assert_eq!(projection.count_for("slot-10am"), 1);
        assert_eq!(projection.count_for("slot-2pm"), 2);
        assert_eq!(projection.total_votes, 3);
        // Votes are ordered by sender for determinism.
        let senders: Vec<&str> = projection
            .votes
            .iter()
            .map(|vote| vote.sender.as_str())
            .collect();
        assert_eq!(senders, vec!["@alice:hs", "@bob:hs", "@carol:hs"]);
    }

    #[test]
    fn redacted_response_reverts_to_previous_vote() {
        let mut newest = response("$v2:hs", "@bob:hs", 300, &["slot-6pm"]);
        newest.redacted = true;
        let projection = reduce_poll(
            &start(1, true),
            &[response("$v1:hs", "@bob:hs", 200, &["slot-10am"]), newest],
            &[],
        )
        .unwrap();
        assert_eq!(projection.vote_of("@bob:hs").unwrap().event_id, "$v1:hs");
        assert_eq!(projection.count_for("slot-10am"), 1);
    }

    #[test]
    fn redacted_only_response_leaves_author_unvoted() {
        let mut only = response("$v1:hs", "@bob:hs", 200, &["slot-10am"]);
        only.redacted = true;
        let projection = reduce_poll(&start(1, true), &[only], &[]).unwrap();
        assert!(projection.vote_of("@bob:hs").is_none());
        assert_eq!(projection.total_votes, 0);
    }

    // ── Ending ────────────────────────────────────────────────────

    #[test]
    fn valid_authorized_end_closes_the_poll() {
        let projection = reduce_poll(
            &start(1, true),
            &[],
            &[end("$e:hs", "@alice:hs", 500, EndAuthorization::Creator)],
        )
        .unwrap();
        assert_eq!(projection.status, PollStatus::Ended);
        let effective = projection.end.expect("ended");
        assert_eq!(effective.event_id, "$e:hs");
        assert_eq!(effective.origin_server_ts, 500);
    }

    #[test]
    fn unauthorized_end_does_not_end_the_poll() {
        let projection = reduce_poll(
            &start(1, true),
            &[],
            &[end(
                "$e:hs",
                "@mallory:hs",
                500,
                EndAuthorization::Unauthorized,
            )],
        )
        .unwrap();
        assert_eq!(projection.status, PollStatus::Open);
        assert!(projection.end.is_none());
    }

    #[test]
    fn redact_power_end_is_valid() {
        let projection = reduce_poll(
            &start(1, true),
            &[],
            &[end("$e:hs", "@mod:hs", 500, EndAuthorization::RedactPower)],
        )
        .unwrap();
        assert_eq!(projection.status, PollStatus::Ended);
    }

    #[test]
    fn redacted_end_is_ignored() {
        let mut redacted = end("$e:hs", "@alice:hs", 500, EndAuthorization::Creator);
        redacted.redacted = true;
        let projection = reduce_poll(&start(1, true), &[], &[redacted]).unwrap();
        assert_eq!(projection.status, PollStatus::Open);
    }

    #[test]
    fn earliest_valid_end_is_effective() {
        let projection = reduce_poll(
            &start(1, true),
            &[],
            &[
                end("$e2:hs", "@alice:hs", 600, EndAuthorization::Creator),
                end("$e1:hs", "@alice:hs", 500, EndAuthorization::Creator),
            ],
        )
        .unwrap();
        assert_eq!(projection.end.unwrap().event_id, "$e1:hs");
    }

    #[test]
    fn later_valid_ends_do_not_replace_the_effective_end() {
        let projection = reduce_poll(
            &start(1, true),
            &[],
            &[
                end("$e1:hs", "@alice:hs", 500, EndAuthorization::Creator),
                end("$e2:hs", "@mod:hs", 900, EndAuthorization::RedactPower),
            ],
        )
        .unwrap();
        assert_eq!(projection.end.unwrap().event_id, "$e1:hs");
    }

    #[test]
    fn equal_timestamp_ends_break_ties_by_event_id() {
        let projection = reduce_poll(
            &start(1, true),
            &[],
            &[
                end("$b:hs", "@alice:hs", 500, EndAuthorization::Creator),
                end("$a:hs", "@mod:hs", 500, EndAuthorization::RedactPower),
            ],
        )
        .unwrap();
        assert_eq!(projection.end.unwrap().event_id, "$a:hs");
    }

    #[test]
    fn an_earlier_unauthorized_end_does_not_preempt_a_later_valid_end() {
        let projection = reduce_poll(
            &start(1, true),
            &[],
            &[
                end(
                    "$bad:hs",
                    "@mallory:hs",
                    400,
                    EndAuthorization::Unauthorized,
                ),
                end("$good:hs", "@alice:hs", 500, EndAuthorization::Creator),
            ],
        )
        .unwrap();
        assert_eq!(projection.end.unwrap().event_id, "$good:hs");
    }

    #[test]
    fn responses_after_the_effective_end_are_ignored() {
        let projection = reduce_poll(
            &start(1, true),
            &[
                response("$before:hs", "@bob:hs", 400, &["slot-10am"]),
                response("$after:hs", "@carol:hs", 600, &["slot-2pm"]),
            ],
            &[end("$e:hs", "@alice:hs", 500, EndAuthorization::Creator)],
        )
        .unwrap();
        assert_eq!(projection.status, PollStatus::Ended);
        assert_eq!(projection.count_for("slot-10am"), 1);
        assert!(
            projection.vote_of("@carol:hs").is_none(),
            "a response after the close is ignored"
        );
        assert_eq!(projection.total_votes, 1);
    }

    #[test]
    fn response_at_the_closing_timestamp_still_counts() {
        let projection = reduce_poll(
            &start(1, true),
            &[response("$at:hs", "@bob:hs", 500, &["slot-10am"])],
            &[end("$e:hs", "@alice:hs", 500, EndAuthorization::Creator)],
        )
        .unwrap();
        assert_eq!(projection.count_for("slot-10am"), 1);
    }

    // ── Determinism & robustness ──────────────────────────────────

    #[test]
    fn reduction_is_independent_of_input_order() {
        let start = start(2, true);
        let responses = vec![
            response("$a:hs", "@alice:hs", 200, &["slot-10am"]),
            response("$b:hs", "@bob:hs", 210, &["slot-2pm", "slot-6pm"]),
            response("$c:hs", "@carol:hs", 220, &[]),
            response("$d:hs", "@dave:hs", 230, &["nope"]),
        ];
        let ends = vec![
            end("$e2:hs", "@mod:hs", 900, EndAuthorization::RedactPower),
            end("$e1:hs", "@alice:hs", 800, EndAuthorization::Creator),
        ];
        let baseline = reduce_poll(&start, &responses, &ends).unwrap();

        let mut shuffled = responses.clone();
        shuffled.reverse();
        let mut shuffled_ends = ends.clone();
        shuffled_ends.reverse();
        assert_eq!(
            reduce_poll(&start, &shuffled, &shuffled_ends).unwrap(),
            baseline
        );

        // Rotations as a second shuffle probe.
        let mut rotated = responses.clone();
        rotated.rotate_left(2);
        assert_eq!(reduce_poll(&start, &rotated, &ends).unwrap(), baseline);
    }

    #[test]
    fn duplicate_delivery_of_the_same_event_changes_nothing() {
        let start = start(2, true);
        let one = response("$a:hs", "@alice:hs", 200, &["slot-10am"]);
        let duplicate = one.clone();
        let single = reduce_poll(&start, std::slice::from_ref(&one), &[]).unwrap();
        let doubled = reduce_poll(&start, &[one, duplicate], &[]).unwrap();
        assert_eq!(single, doubled);
        assert_eq!(doubled.total_votes, 1);
        assert_eq!(doubled.count_for("slot-10am"), 1);

        let end_one = end("$e:hs", "@alice:hs", 500, EndAuthorization::Creator);
        let end_dupe = end_one.clone();
        let single_end = reduce_poll(&start, &[], std::slice::from_ref(&end_one)).unwrap();
        let doubled_end = reduce_poll(&start, &[], &[end_one, end_dupe]).unwrap();
        assert_eq!(single_end, doubled_end);
    }

    #[test]
    fn malformed_independent_events_do_not_corrupt_valid_votes() {
        // A spoiled response and an unauthorized end leave unrelated authors
        // and the poll status untouched.
        let projection = reduce_poll(
            &start(2, true),
            &[
                response("$good:hs", "@alice:hs", 200, &["slot-10am"]),
                response("$bad:hs", "@bob:hs", 201, &["unknown-id"]),
            ],
            &[end(
                "$e:hs",
                "@mallory:hs",
                300,
                EndAuthorization::Unauthorized,
            )],
        )
        .unwrap();
        assert_eq!(projection.status, PollStatus::Open);
        assert_eq!(projection.count_for("slot-10am"), 1);
        assert_eq!(projection.total_votes, 1);
        assert!(projection.vote_of("@bob:hs").unwrap().spoiled);
    }

    #[test]
    fn multi_select_tallies_are_per_answer() {
        let projection = reduce_poll(
            &start(2, true),
            &[
                response("$a:hs", "@alice:hs", 200, &["slot-10am", "slot-2pm"]),
                response("$b:hs", "@bob:hs", 201, &["slot-2pm"]),
            ],
            &[],
        )
        .unwrap();
        assert_eq!(projection.count_for("slot-10am"), 1);
        assert_eq!(projection.count_for("slot-2pm"), 2);
        assert_eq!(projection.count_for("slot-6pm"), 0);
        assert_eq!(projection.total_votes, 2);
    }

    #[test]
    fn empty_inputs_produce_an_open_poll() {
        let projection = reduce_poll(&start(1, true), &[], &[]).unwrap();
        assert_eq!(projection.status, PollStatus::Open);
        assert!(projection.votes.is_empty());
        assert_eq!(projection.total_votes, 0);
    }

    #[test]
    fn end_authorization_from_facts_is_creator_only() {
        // The creator check is decidable from the canonical facts.
        assert_eq!(
            EndAuthorization::from_facts("@alice:hs", "@alice:hs"),
            EndAuthorization::Creator
        );
        assert!(EndAuthorization::from_facts("@alice:hs", "@alice:hs").is_authorized());
        // Any other sender is the explicit unresolved boundary: no current
        // room-power snapshot may turn it into a permanent fact.
        assert_eq!(
            EndAuthorization::from_facts("@alice:hs", "@mod:hs"),
            EndAuthorization::Unauthorized
        );
        assert!(!EndAuthorization::from_facts("@alice:hs", "@mod:hs").is_authorized());
    }

    #[test]
    fn reduce_poll_is_deterministic_across_fact_orderings() {
        let poll_start = start(2, true);
        let responses = vec![
            PollResponseFact::new("$r1:hs", "@alice:hs", 110, vec!["slot-10am".to_string()]),
            PollResponseFact::new("$r2:hs", "@alice:hs", 120, vec!["slot-2pm".to_string()]),
            PollResponseFact::new(
                "$r3:hs",
                "@bob:hs",
                115,
                vec!["slot-10am".to_string(), "slot-2pm".to_string()],
            ),
            PollResponseFact::new("$r4:hs", "@charlie:hs", 130, vec!["slot-6pm".to_string()]),
        ];
        let ends = vec![
            PollEndFact {
                event_id: "$end2:hs".to_string(),
                sender: "@alice:hs".to_string(),
                origin_server_ts: 150,
                authorization: EndAuthorization::Creator,
                redacted: false,
            },
            PollEndFact {
                event_id: "$end1:hs".to_string(),
                sender: "@alice:hs".to_string(),
                origin_server_ts: 140,
                authorization: EndAuthorization::Creator,
                redacted: false,
            },
        ];

        let baseline = reduce_poll(&poll_start, &responses, &ends).expect("reduce baseline");

        // Reversed responses and ends
        let mut rev_responses = responses.clone();
        rev_responses.reverse();
        let mut rev_ends = ends.clone();
        rev_ends.reverse();
        let from_rev = reduce_poll(&poll_start, &rev_responses, &rev_ends).expect("reduce rev");
        assert_eq!(
            baseline, from_rev,
            "reversed fact order produces identical projection"
        );

        // Responses with duplicated delivery
        let mut dup_responses = responses.clone();
        dup_responses.extend(responses.clone());
        let mut dup_ends = ends.clone();
        dup_ends.extend(ends.clone());
        let from_dup = reduce_poll(&poll_start, &dup_responses, &dup_ends).expect("reduce dup");
        assert_eq!(
            baseline, from_dup,
            "duplicated fact delivery produces identical projection"
        );
    }
}

#[cfg(test)]
mod semantic_tests {
    use super::*;
    use crate::canonical::CanonicalJson;

    fn answers() -> Vec<PollSemanticAnswer> {
        vec![
            PollSemanticAnswer::new("slot-10am", "10:00 AM UTC"),
            PollSemanticAnswer::new("slot-2pm", "2:00 PM UTC"),
        ]
    }

    #[test]
    fn semantic_operation_matches_the_frozen_structure() {
        let op = poll_semantic_operation(
            "site-dev",
            "post-101",
            None,
            None,
            "Which meeting time works best?",
            &answers(),
            PollSemanticKind::Disclosed,
            1,
        )
        .expect("semantic operation");
        assert_eq!(
            op.to_canonical_string(),
            r#"["POLL",["site-dev","post-101",null,null],["Which meeting time works best?",[["slot-10am","10:00 AM UTC"],["slot-2pm","2:00 PM UTC"]],"disclosed",1],1]"#
        );
    }

    #[test]
    fn semantic_operation_preserves_answer_order_and_relations() {
        let op = poll_semantic_operation(
            "site",
            "page",
            Some("$parent:hs"),
            Some("$thread:hs"),
            "q?",
            &answers(),
            PollSemanticKind::Undisclosed,
            2,
        )
        .expect("semantic operation");
        let json = op.to_json_value();
        assert_eq!(
            json[1],
            serde_json::json!(["site", "page", "$parent:hs", "$thread:hs"])
        );
        assert_eq!(json[2][1][0][0], "slot-10am");
        assert_eq!(json[2][1][1][0], "slot-2pm");
        assert_eq!(json[2][2], "undisclosed");
        assert_eq!(json[2][3], 2);
        assert_eq!(json[3], 1);

        // Reordering answers changes the canonical value (never sorted).
        let mut reordered = answers();
        reordered.reverse();
        let swapped = poll_semantic_operation(
            "site",
            "page",
            Some("$parent:hs"),
            Some("$thread:hs"),
            "q?",
            &reordered,
            PollSemanticKind::Undisclosed,
            2,
        )
        .expect("semantic operation");
        assert_ne!(op, swapped);
    }

    #[test]
    fn fingerprint_is_stable_and_sensitive_to_semantics() {
        let op = poll_semantic_operation(
            "site",
            "page",
            None,
            None,
            "q?",
            &answers(),
            PollSemanticKind::Disclosed,
            1,
        )
        .expect("semantic operation");
        let fp = poll_semantic_fingerprint(&op);
        assert_eq!(fp, poll_semantic_fingerprint(&op));
        assert_eq!(fp.len(), 64);

        let changed = poll_semantic_operation(
            "site",
            "page",
            None,
            None,
            "q?!",
            &answers(),
            PollSemanticKind::Disclosed,
            1,
        )
        .expect("semantic operation");
        assert_ne!(fp, poll_semantic_fingerprint(&changed));
    }

    #[test]
    fn envelope_binds_operation_id_and_challenge() {
        let op = poll_semantic_operation(
            "site",
            "page",
            None,
            None,
            "q?",
            &answers(),
            PollSemanticKind::Disclosed,
            1,
        )
        .expect("semantic operation");
        let envelope = poll_signature_envelope(&op, "op-create-987", "pow-chal-550e8400");
        assert_eq!(
            envelope.to_canonical_string(),
            format!(
                r#"["host.curious.cumments.signature","1",{},"op-create-987","pow-chal-550e8400"]"#,
                op.to_canonical_string()
            )
        );
        // The public key is not part of the envelope.
        assert!(!envelope.to_canonical_string().contains("public_key"));
    }

    #[test]
    fn signature_roundtrips_and_rejects_any_tamper() {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        use ed25519_dalek::{Signer, SigningKey};

        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
        let op = poll_semantic_operation(
            "site",
            "page",
            None,
            None,
            "q?",
            &answers(),
            PollSemanticKind::Disclosed,
            1,
        )
        .expect("semantic operation");
        let envelope = poll_signature_envelope(&op, "op-1", "chal");
        let signature = URL_SAFE_NO_PAD.encode(
            signing_key
                .sign(envelope.to_canonical_bytes().as_slice())
                .to_bytes(),
        );

        assert!(verify_poll_signature(
            &public_key,
            &op,
            "op-1",
            "chal",
            &signature
        ));

        // Modified question.
        let tampered_question = poll_semantic_operation(
            "site",
            "page",
            None,
            None,
            "other?",
            &answers(),
            PollSemanticKind::Disclosed,
            1,
        )
        .expect("semantic operation");
        assert!(!verify_poll_signature(
            &public_key,
            &tampered_question,
            "op-1",
            "chal",
            &signature
        ));

        // Modified answers.
        let mut changed_answers = answers();
        changed_answers[0].text = "10:00".to_string();
        let tampered_answers = poll_semantic_operation(
            "site",
            "page",
            None,
            None,
            "q?",
            &changed_answers,
            PollSemanticKind::Disclosed,
            1,
        )
        .expect("semantic operation");
        assert!(!verify_poll_signature(
            &public_key,
            &tampered_answers,
            "op-1",
            "chal",
            &signature
        ));

        // Modified target.
        let tampered_target = poll_semantic_operation(
            "site",
            "other-page",
            None,
            None,
            "q?",
            &answers(),
            PollSemanticKind::Disclosed,
            1,
        )
        .expect("semantic operation");
        assert!(!verify_poll_signature(
            &public_key,
            &tampered_target,
            "op-1",
            "chal",
            &signature
        ));

        // Modified operation id and challenge.
        assert!(!verify_poll_signature(
            &public_key,
            &op,
            "op-2",
            "chal",
            &signature
        ));
        assert!(!verify_poll_signature(
            &public_key,
            &op,
            "op-1",
            "chal2",
            &signature
        ));
    }

    #[test]
    fn definition_validation_enforces_the_semantic_contract() {
        assert!(validate_poll_semantic_definition("q?", &answers(), 1).is_ok());
        assert!(validate_poll_semantic_definition("q?", &answers(), 2).is_ok());

        // Empty question.
        assert_eq!(
            validate_poll_semantic_definition("  ", &answers(), 1),
            Err(PollSemanticError::EmptyQuestion)
        );
        // Answer count: one answer is a valid poll, more than the Matrix Poll
        // limit is rejected rather than truncated.
        assert!(validate_poll_semantic_definition("q?", &answers()[..1], 1).is_ok());
        let too_many: Vec<PollSemanticAnswer> = (0..21)
            .map(|i| PollSemanticAnswer::new(format!("id{i}"), "t"))
            .collect();
        assert_eq!(
            validate_poll_semantic_definition("q?", &too_many, 1),
            Err(PollSemanticError::AnswerCount)
        );
        let exactly_twenty: Vec<PollSemanticAnswer> = (0..20)
            .map(|i| PollSemanticAnswer::new(format!("id{i}"), "t"))
            .collect();
        assert!(validate_poll_semantic_definition("q?", &exactly_twenty, 1).is_ok());

        // Answer ids are opaque: no syntax, length, or emptiness rule.
        let opaque_ids = vec![
            PollSemanticAnswer::new("", "Empty id"),
            PollSemanticAnswer::new("has space", "Spaced"),
            PollSemanticAnswer::new("slash/id", "Slashed"),
            PollSemanticAnswer::new("🙂", "Emoji"),
            PollSemanticAnswer::new("x".repeat(65), "Long"),
        ];
        assert!(validate_poll_semantic_definition("q?", &opaque_ids, 1).is_ok());

        // Duplicate ids are still rejected, by exact string equality.
        let dup = vec![
            PollSemanticAnswer::new("a", "A"),
            PollSemanticAnswer::new("a", "A again"),
        ];
        assert_eq!(
            validate_poll_semantic_definition("q?", &dup, 1),
            Err(PollSemanticError::DuplicateAnswerId("a".into()))
        );
        let dup_empty = vec![
            PollSemanticAnswer::new("", "First"),
            PollSemanticAnswer::new("", "Second"),
        ];
        assert_eq!(
            validate_poll_semantic_definition("q?", &dup_empty, 1),
            Err(PollSemanticError::DuplicateAnswerId(String::new()))
        );

        // Empty answer text.
        let empty_text = vec![
            PollSemanticAnswer::new("a", " "),
            PollSemanticAnswer::new("b", "B"),
        ];
        assert_eq!(
            validate_poll_semantic_definition("q?", &empty_text, 1),
            Err(PollSemanticError::EmptyAnswerText)
        );

        // max_selections: at least 1, otherwise unbounded by the application.
        assert_eq!(
            validate_poll_semantic_definition("q?", &answers(), 0),
            Err(PollSemanticError::MaxSelections)
        );
        assert!(validate_poll_semantic_definition("q?", &answers(), 1).is_ok());
        // More selections than answers is a legitimate multi-select poll.
        assert!(validate_poll_semantic_definition("q?", &answers(), 3).is_ok());
        assert!(validate_poll_semantic_definition("q?", &answers(), 20).is_ok());
        // The only upper bound is the canonical integer representation.
        assert!(
            validate_poll_semantic_definition(
                "q?",
                &answers(),
                crate::canonical::MAX_SAFE_CANONICAL_INT as u64
            )
            .is_ok()
        );
        assert_eq!(
            validate_poll_semantic_definition(
                "q?",
                &answers(),
                crate::canonical::MAX_SAFE_CANONICAL_INT as u64 + 1
            ),
            Err(PollSemanticError::MaxSelectionsNotRepresentable(
                crate::canonical::MAX_SAFE_CANONICAL_INT as u64 + 1
            ))
        );
    }

    #[test]
    fn long_question_and_answer_text_have_no_application_maximum() {
        let long_question = "q".repeat(501);
        let long_answer_text = "t".repeat(201);
        let long_answers = vec![
            PollSemanticAnswer::new("a", long_answer_text.clone()),
            PollSemanticAnswer::new("b", "B"),
        ];
        assert!(validate_poll_semantic_definition(&long_question, &long_answers, 1).is_ok());

        // And the values reach the signed operation unshortened.
        let operation = poll_semantic_operation(
            "site",
            "page",
            None,
            None,
            &long_question,
            &long_answers,
            PollSemanticKind::Disclosed,
            1,
        )
        .expect("validated definition");
        let json = operation.to_json_value();
        assert_eq!(json[2][0], serde_json::json!(long_question));
        assert_eq!(json[2][1][0][1], serde_json::json!(long_answer_text));
    }

    #[test]
    fn unrepresentable_max_selections_never_reaches_a_signed_operation() {
        assert!(
            poll_semantic_operation(
                "site",
                "page",
                None,
                None,
                "q?",
                &answers(),
                PollSemanticKind::Disclosed,
                crate::canonical::MAX_SAFE_CANONICAL_INT as u64,
            )
            .is_some()
        );
        assert!(
            poll_semantic_operation(
                "site",
                "page",
                None,
                None,
                "q?",
                &answers(),
                PollSemanticKind::Disclosed,
                crate::canonical::MAX_SAFE_CANONICAL_INT as u64 + 1,
            )
            .is_none()
        );
    }

    #[test]
    fn kind_wire_values_are_bare() {
        assert_eq!(PollSemanticKind::Disclosed.as_str(), "disclosed");
        assert_eq!(PollSemanticKind::Undisclosed.as_str(), "undisclosed");
        assert_eq!(
            serde_json::to_value(PollSemanticKind::Disclosed).unwrap(),
            CanonicalJson::string("disclosed").to_json_value()
        );
    }

    #[test]
    fn poll_start_proof_requires_signed_operation_to_match_wire_content() {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        use ed25519_dalek::{Signer, SigningKey};

        let signing_key = SigningKey::from_bytes(&[41u8; 32]);
        let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
        let wire = PollWireSemantics {
            site_id: "my-blog".to_string(),
            page_slug: "hello".to_string(),
            reply_to: None,
            thread_root: None,
            question: "q?".to_string(),
            answers: vec![
                PollSemanticAnswer::new("a", "A"),
                PollSemanticAnswer::new("b", "B"),
            ],
            kind: PollSemanticKind::Disclosed,
            max_selections: 1,
        };
        // Sign exactly the operation the wire content denotes.
        let signed = wire.to_semantic_operation().expect("representable");
        let envelope = poll_signature_envelope(&signed, "op-1", "chal");
        let signature = URL_SAFE_NO_PAD.encode(
            signing_key
                .sign(envelope.to_canonical_bytes().as_slice())
                .to_bytes(),
        );

        assert!(verify_poll_start_proof(
            &public_key,
            &signed,
            "op-1",
            "chal",
            &signature,
            &wire
        ));

        // A different wire Poll (same valid signature) must not verify.
        let tampered = PollWireSemantics {
            question: "evil?".to_string(),
            ..wire.clone()
        };
        assert!(
            !verify_poll_start_proof(&public_key, &signed, "op-1", "chal", &signature, &tampered),
            "signed operation must match the actual wire Poll"
        );

        // Answer order is semantic: swapping the wire answers must not verify.
        let reordered = PollWireSemantics {
            answers: vec![
                PollSemanticAnswer::new("b", "B"),
                PollSemanticAnswer::new("a", "A"),
            ],
            ..wire.clone()
        };
        assert!(!verify_poll_start_proof(
            &public_key,
            &signed,
            "op-1",
            "chal",
            &signature,
            &reordered
        ));

        // A signed operation that does not match the wire is rejected even when
        // it is otherwise internally consistent and correctly signed.
        let other = PollWireSemantics {
            kind: PollSemanticKind::Undisclosed,
            ..wire.clone()
        };
        let other_op = other.to_semantic_operation().expect("representable");
        let other_envelope = poll_signature_envelope(&other_op, "op-1", "chal");
        let other_sig = URL_SAFE_NO_PAD.encode(
            signing_key
                .sign(other_envelope.to_canonical_bytes().as_slice())
                .to_bytes(),
        );
        assert!(!verify_poll_start_proof(
            &public_key,
            &other_op,
            "op-1",
            "chal",
            &other_sig,
            &wire
        ));
    }

    // ── Vote ──────────────────────────────────────────────────────

    #[test]
    fn vote_selections_are_canonicalized_as_a_set() {
        let known: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let norm = |raw: &[&str]| {
            let raw: Vec<String> = raw.iter().map(|s| s.to_string()).collect();
            normalize_vote_selections(&raw, 2, &known)
        };

        assert_eq!(norm(&["a"]).unwrap(), vec!["a"]);
        assert_eq!(norm(&["b", "a"]).unwrap(), vec!["a", "b"]);
        assert_eq!(norm(&["a", "a", "b"]).unwrap(), vec!["a", "b"]);
        // Empty is an explicit unvote.
        assert_eq!(norm(&[]).unwrap(), Vec::<String>::new());

        // Case-sensitive: "A" and "a" are distinct identifiers.
        let case_known: Vec<String> = ["a", "A"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            normalize_vote_selections(&["A".to_string(), "a".to_string()], 2, &case_known).unwrap(),
            vec!["A", "a"]
        );

        // Opaque ids are selections like any other, including the empty string.
        let opaque_known: Vec<String> = ["", "has space", "🙂"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            normalize_vote_selections(
                &[
                    "🙂".to_string(),
                    "has space".to_string(),
                    String::new(),
                    "has space".to_string(),
                ],
                3,
                &opaque_known,
            )
            .unwrap(),
            vec![String::new(), "has space".to_string(), "🙂".to_string()]
        );
    }

    #[test]
    fn vote_selection_conflicts_are_rejected() {
        let known: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        // Unknown id. Note there is no syntax stage: an id the poll declares is
        // accepted whatever it contains, and one it does not declare is unknown.
        assert_eq!(
            normalize_vote_selections(&["zzz".to_string()], 1, &known),
            Err(VoteSelectionError::UnknownId("zzz".to_string()))
        );
        assert_eq!(
            normalize_vote_selections(&["has space".to_string()], 1, &known),
            Err(VoteSelectionError::UnknownId("has space".to_string()))
        );
        assert_eq!(
            normalize_vote_selections(&[String::new()], 1, &known),
            Err(VoteSelectionError::UnknownId(String::new()))
        );
        // Too many selections after deduplication (["a","a","b"] -> 2 > 1).
        assert_eq!(
            normalize_vote_selections(
                &["a".to_string(), "a".to_string(), "b".to_string()],
                1,
                &known
            ),
            Err(VoteSelectionError::TooMany { max: 1 })
        );
        // Duplicates do not consume slots after deduplication.
        assert_eq!(
            normalize_vote_selections(&["a".to_string(), "a".to_string()], 1, &known),
            Ok(vec!["a".to_string()])
        );
    }

    #[test]
    fn vote_semantic_operation_matches_the_frozen_structure() {
        let op = vote_semantic_operation(
            "site-dev",
            "post-101",
            "$poll:hs",
            &["slot-10am".to_string()],
        );
        assert_eq!(
            op.to_canonical_string(),
            r#"["VOTE",["site-dev","post-101","$poll:hs"],["slot-10am"],1]"#
        );
    }

    #[test]
    fn vote_operation_is_order_insensitive_but_binds_target_and_set() {
        let a = vote_semantic_operation(
            "site",
            "page",
            "$poll:hs",
            &["a".to_string(), "b".to_string()],
        );
        // A permutation that normalizes to the same set produces the same
        // operation and fingerprint.
        let known = vec!["a".to_string(), "b".to_string()];
        let permuted_ids =
            normalize_vote_selections(&["b".to_string(), "a".to_string()], 2, &known).unwrap();
        let permuted = vote_semantic_operation("site", "page", "$poll:hs", &permuted_ids);
        assert_eq!(a, permuted);
        assert_eq!(
            poll_semantic_fingerprint(&a),
            poll_semantic_fingerprint(&permuted)
        );

        // Target changes the operation.
        let other_target = vote_semantic_operation(
            "site",
            "page",
            "$other:hs",
            &["a".to_string(), "b".to_string()],
        );
        assert_ne!(a, other_target);
        // Selection set changes the operation.
        let other_set = vote_semantic_operation("site", "page", "$poll:hs", &["a".to_string()]);
        assert_ne!(a, other_set);
        // Schema version is the final element.
        let json = a.to_json_value();
        assert_eq!(json[0], "VOTE");
        assert_eq!(json[1], serde_json::json!(["site", "page", "$poll:hs"]));
        assert_eq!(json[2], serde_json::json!(["a", "b"]));
        assert_eq!(json[3], 1);
    }

    #[test]
    fn vote_proof_verifies_signature_and_wire_consistency() {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        use ed25519_dalek::{Signer, SigningKey};

        let signing_key = SigningKey::from_bytes(&[51u8; 32]);
        let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
        let wire = VoteWireSemantics {
            site_id: "site".to_string(),
            page_slug: "page".to_string(),
            poll_event_id: "$poll:hs".to_string(),
            option_ids: vec!["a".to_string()],
        };
        let signed = wire.to_semantic_operation();
        let envelope = poll_signature_envelope(&signed, "op-1", "chal");
        let signature = URL_SAFE_NO_PAD.encode(
            signing_key
                .sign(envelope.to_canonical_bytes().as_slice())
                .to_bytes(),
        );

        assert!(verify_vote_proof(
            &public_key,
            &signed,
            "op-1",
            "chal",
            &signature,
            &wire
        ));

        // A different selection set must not verify against the same signature.
        let tampered = VoteWireSemantics {
            option_ids: vec!["b".to_string()],
            ..wire.clone()
        };
        assert!(!verify_vote_proof(
            &public_key,
            &signed,
            "op-1",
            "chal",
            &signature,
            &tampered
        ));
        // Target changes are rejected too.
        let other_target = VoteWireSemantics {
            poll_event_id: "$other:hs".to_string(),
            ..wire.clone()
        };
        assert!(!verify_vote_proof(
            &public_key,
            &signed,
            "op-1",
            "chal",
            &signature,
            &other_target
        ));
    }

    #[test]
    fn end_poll_semantic_operation_matches_the_frozen_structure() {
        let op = end_poll_semantic_operation("site-dev", "post-101", "$poll:hs");
        assert_eq!(
            op.to_canonical_string(),
            r#"["END_POLL",["site-dev","post-101","$poll:hs"],[],1]"#
        );
        let json = op.to_json_value();
        assert_eq!(json[0], "END_POLL");
        assert_eq!(
            json[1],
            serde_json::json!(["site-dev", "post-101", "$poll:hs"])
        );
        assert_eq!(json[2], serde_json::json!([]));
        assert_eq!(json[3], 1);
    }

    #[test]
    fn end_poll_fingerprint_binds_target_and_excludes_metadata() {
        let a = end_poll_semantic_operation("site", "page", "$poll:hs");
        let b = end_poll_semantic_operation("site", "page", "$other:hs");
        let c = end_poll_semantic_operation("site2", "page", "$poll:hs");
        let d = end_poll_semantic_operation("site", "page2", "$poll:hs");

        let fp_a = poll_semantic_fingerprint(&a);
        let fp_b = poll_semantic_fingerprint(&b);
        let fp_c = poll_semantic_fingerprint(&c);
        let fp_d = poll_semantic_fingerprint(&d);

        assert_ne!(fp_a, fp_b);
        assert_ne!(fp_a, fp_c);
        assert_ne!(fp_a, fp_d);
    }

    #[test]
    fn end_poll_proof_verifies_signature_and_wire_consistency() {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        use ed25519_dalek::{Signer, SigningKey};

        let signing_key = SigningKey::from_bytes(&[61u8; 32]);
        let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
        let wire = EndPollWireSemantics {
            site_id: "site".to_string(),
            page_slug: "page".to_string(),
            poll_event_id: "$poll:hs".to_string(),
        };
        let signed = wire.to_semantic_operation();
        let envelope = poll_signature_envelope(&signed, "op-end-1", "chal-1");
        let signature = URL_SAFE_NO_PAD.encode(
            signing_key
                .sign(envelope.to_canonical_bytes().as_slice())
                .to_bytes(),
        );

        assert!(verify_end_poll_proof(
            &public_key,
            &signed,
            "op-end-1",
            "chal-1",
            &signature,
            &wire
        ));

        // Different operation_id rejects.
        assert!(!verify_end_poll_proof(
            &public_key,
            &signed,
            "op-end-2",
            "chal-1",
            &signature,
            &wire
        ));

        // Different challenge rejects.
        assert!(!verify_end_poll_proof(
            &public_key,
            &signed,
            "op-end-1",
            "chal-2",
            &signature,
            &wire
        ));

        // Tampered target rejects.
        let tampered = EndPollWireSemantics {
            poll_event_id: "$other:hs".to_string(),
            ..wire
        };
        assert!(!verify_end_poll_proof(
            &public_key,
            &signed,
            "op-end-1",
            "chal-1",
            &signature,
            &tampered
        ));
    }
}
