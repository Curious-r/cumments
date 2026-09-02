//! The commands of the system: serializable, signature-verified declarations
//! of a write the user wants performed. The API layer writes a command into
//! the submission queue; the reconciler executes it against Matrix. The
//! user's *intent* is a mental concept — commands are its concrete form.

use crate::models::{CommentMedia, PageSlug, SiteId};
use serde::{Deserialize, Serialize};

fn default_poll_max_selections() -> u8 {
    1
}

/// Represents the user's desire to post a comment.
/// This is a command to be processed asynchronously by the reconciler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostCommentCommand {
    /// The site this comment belongs to.
    pub site_id: SiteId,
    /// The post/page this comment belongs to.
    pub page_slug: PageSlug,

    /// The content of the comment, likely in Markdown.
    pub content: String,
    /// Optional media attachment (image/voice/file); when present, `content`
    /// carries the fallback filename and the signature covers the media URL.
    #[serde(default)]
    pub media: Option<CommentMedia>,
    /// A location message (MSC3488) instead of text/media. When present,
    /// `content` is unused and the signature covers `location.geo_uri`
    /// plus the orthogonal relations `reply_to` / `thread_root`.
    #[serde(default)]
    pub location: Option<LocationPayload>,
    /// A poll (MSC3381) instead of text/media/location. When present,
    /// `content` is unused and the signature covers the poll payload
    /// (`question`, ordered `options`, `max_selections`) plus
    /// the orthogonal relations `reply_to` / `thread_root`.
    #[serde(default)]
    pub poll: Option<PollPayload>,

    /// Display name of the author. For visitors, this is provided by them.
    pub display_name: String,

    /// Ed25519 public key identifying the author (base64url).
    /// Ownership is publicly verifiable from Matrix events.
    pub author_public_key: String,
    /// Ed25519 signature over the canonical request message.
    pub author_signature: String,
    /// PoW challenge prefix included in the signed message. Published in the
    /// Matrix event so the signature remains independently verifiable.
    #[serde(default)]
    pub author_challenge: String,

    /// If this comment is a reply, this field holds the ID of the parent comment.
    pub reply_to: Option<String>,
    /// Thread root event ID (`m.thread`). Orthogonal to `reply_to`.
    pub thread_root: Option<String>,
}

/// Payload for a visitor location message (`m.location`, MSC3488).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocationPayload {
    pub geo_uri: String,
    pub description: Option<String>,
}

/// Payload for a visitor poll (`m.poll.start`, MSC3381).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollPayload {
    pub question: String,
    pub options: Vec<String>,
    #[serde(default = "default_poll_max_selections")]
    pub max_selections: u8,
}

/// Represents the user's desire to delete a comment.
/// This is a command to be processed asynchronously by the reconciler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteCommentCommand {
    /// The site this comment belongs to.
    pub site_id: SiteId,
    /// The post/page this comment belongs to.
    pub page_slug: PageSlug,
    /// The Matrix event ID of the comment to be deleted.
    pub event_id: String,
    /// The author's Ed25519 public key (base64url).
    pub author_public_key: String,
    /// Ed25519 signature authorizing this deletion.
    pub author_signature: String,
    /// PoW challenge prefix included in the signed message. Published in the
    /// redaction reason so the signature remains independently verifiable
    /// from the event log.
    #[serde(default)]
    pub author_challenge: String,
}

/// Represents the user's desire to edit/update a comment.
/// This is a command to be processed asynchronously by the reconciler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCommentCommand {
    /// The site this comment belongs to.
    pub site_id: SiteId,
    /// The post/page this comment belongs to.
    pub page_slug: PageSlug,
    /// The Matrix event ID of the comment to be updated.
    pub event_id: String,
    /// The new content for the comment.
    pub content: String,
    /// The author's Ed25519 public key (base64url).
    pub author_public_key: String,
    /// Ed25519 signature authorizing this edit.
    pub author_signature: String,
    /// PoW challenge prefix included in the signed message. Published in the
    /// replacement event so the signature remains independently verifiable.
    #[serde(default)]
    pub author_challenge: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_post_command_without_poll_deserializes() {
        let json = r#"{
            "site_id":"my-blog",
            "page_slug":"hello",
            "content":"hi",
            "display_name":"Alice",
            "author_public_key":"pk",
            "author_signature":"sig",
            "author_challenge":"chal"
        }"#;
        let cmd: PostCommentCommand =
            serde_json::from_str(json).expect("old payload must deserialize");
        assert!(cmd.poll.is_none());
        assert!(cmd.location.is_none());
        assert!(cmd.media.is_none());
        // Re-serializing and re-parsing must preserve the absence.
        let round = serde_json::to_string(&cmd).unwrap();
        let again: PostCommentCommand = serde_json::from_str(&round).unwrap();
        assert!(again.poll.is_none());
    }

    #[test]
    fn poll_payload_round_trips() {
        let cmd = PostCommentCommand {
            site_id: SiteId::from("my-blog"),
            page_slug: PageSlug::from("hello"),
            content: String::new(),
            media: None,
            location: None,
            poll: Some(PollPayload {
                question: "Best?".to_string(),
                options: vec!["A".to_string(), "B".to_string()],
                max_selections: 1,
            }),
            display_name: "Alice".to_string(),
            author_public_key: "pk".to_string(),
            author_signature: "sig".to_string(),
            author_challenge: "chal".to_string(),
            reply_to: None,
            thread_root: None,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: PostCommentCommand = serde_json::from_str(&json).unwrap();
        let poll = back.poll.expect("poll must survive round-trip");
        assert_eq!(poll.question, "Best?");
        assert_eq!(poll.options, vec!["A", "B"]);
        assert_eq!(poll.max_selections, 1);
    }

    #[test]
    fn poll_max_selections_defaults_to_one() {
        let json = r#"{"question":"q","options":["a","b"]}"#;
        let payload: PollPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.max_selections, 1);
        let with_explicit: PollPayload =
            serde_json::from_str(r#"{"question":"q","options":["a","b"],"max_selections":1}"#)
                .unwrap();
        assert_eq!(with_explicit.max_selections, 1);
    }
}
