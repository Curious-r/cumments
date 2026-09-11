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

use serde::{Deserialize, Serialize};

/// Maximum length of an answer identifier, in characters.
pub const MAX_ANSWER_ID_LEN: usize = 64;

/// Whether a string is a valid answer identifier.
///
/// Answer identifiers are opaque, case-sensitive tokens of 1 to 64 visible
/// ASCII characters from `[a-zA-Z0-9_.-]`, without whitespace or control
/// codes (frozen design §11.2).
pub fn is_valid_answer_id(id: &str) -> bool {
    let len = id.chars().count();
    (1..=MAX_ANSWER_ID_LEN).contains(&len)
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
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
    #[error("poll answer id {0:?} is not a valid answer identifier")]
    InvalidAnswerId(String),
    #[error("poll answer id {0:?} is duplicated")]
    DuplicateAnswerId(String),
    #[error("poll max_selections must be at least 1")]
    ZeroMaxSelections,
}

impl PollStartFact {
    /// Validate the poll definition, following the frozen design's answer-id
    /// rules (§11.2). Answer order is preserved and answer ids are compared
    /// byte-for-byte (case-sensitive).
    pub fn validate(&self) -> Result<(), PollStartError> {
        if self.max_selections < 1 {
            return Err(PollStartError::ZeroMaxSelections);
        }
        if self.answers.is_empty() {
            return Err(PollStartError::NoAnswers);
        }
        for (index, answer) in self.answers.iter().enumerate() {
            if !is_valid_answer_id(&answer.id) {
                return Err(PollStartError::InvalidAnswerId(answer.id.clone()));
            }
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
    fn invalid_answer_id_syntax_is_rejected() {
        for bad in ["", "with space", "emoji-🙂", "tab\tid", "slash/id"] {
            let mut fact = start(1, true);
            fact.answers = vec![PollAnswerFact::new(bad, "X")];
            assert_eq!(
                reduce_poll(&fact, &[], &[]),
                Err(PollStartError::InvalidAnswerId(bad.to_string())),
                "{bad:?} must be rejected"
            );
        }
        // 65 characters is over the limit.
        let too_long = "x".repeat(MAX_ANSWER_ID_LEN + 1);
        let mut fact = start(1, true);
        fact.answers = vec![PollAnswerFact::new(too_long.clone(), "X")];
        assert_eq!(
            reduce_poll(&fact, &[], &[]),
            Err(PollStartError::InvalidAnswerId(too_long))
        );
        // Exactly 64 characters is allowed.
        let mut fact = start(1, true);
        fact.answers = vec![PollAnswerFact::new("x".repeat(MAX_ANSWER_ID_LEN), "X")];
        assert!(reduce_poll(&fact, &[], &[]).is_ok());
    }

    #[test]
    fn valid_answer_id_alphabet_is_accepted() {
        for id in ["a", "A", "0", "slot-10am", "a_b.c-d", "Z9"] {
            assert!(is_valid_answer_id(id), "{id:?} must be valid");
        }
        assert!(!is_valid_answer_id(""));
        assert!(!is_valid_answer_id(" spaces "));
    }

    #[test]
    fn zero_max_selections_is_rejected() {
        assert_eq!(
            reduce_poll(&start(0, true), &[], &[]),
            Err(PollStartError::ZeroMaxSelections)
        );
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
}
