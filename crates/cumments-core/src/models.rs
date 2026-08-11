//! Defines the core data models of the application.
//! These models should be pure data structures with no logic tied to infrastructure.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use validator::Validate;

lazy_static::lazy_static! {
    /// Allowed chars: lowercase a-z, 0-9, hyphen.
    ///
    /// Uppercase and underscores are excluded deliberately: `site_id` and
    /// `post_slug` are embedded in Matrix user IDs and room aliases, where
    /// lowercase keeps user IDs spec-compliant and `_` stays a safe separator
    /// in `#_cumments_{site}_{post}` aliases.
    /// Length: 1–64 characters.
    pub static ref ID_REGEX: regex::Regex =
        regex::Regex::new(r"^[a-z0-9-]{1,64}$").unwrap();
}

// A validated, owned representation of a Site ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Validate)]
#[serde(transparent)]
pub struct SiteId {
    #[validate(regex(path = "*crate::models::ID_REGEX"))]
    pub id: String,
}

impl SiteId {
    /// Creates a new `SiteId` with validation.
    /// Returns `ValidationErrors` if the input doesn't match the expected format.
    pub fn new(id: String) -> Result<Self, validator::ValidationErrors> {
        let this = Self { id };
        this.validate()?;
        Ok(this)
    }

    pub fn as_str(&self) -> &str {
        &self.id
    }
}

// Internal use only – data must already be validated.
// For untrusted input, use `SiteId::new()` which runs validation.
impl From<String> for SiteId {
    fn from(id: String) -> Self {
        Self { id }
    }
}

impl From<&str> for SiteId {
    fn from(s: &str) -> Self {
        Self { id: s.to_string() }
    }
}

// A validated, owned representation of a Post Slug.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Validate)]
#[serde(transparent)]
pub struct PostSlug {
    #[validate(regex(path = "*crate::models::ID_REGEX"))]
    pub slug: String,
}

impl PostSlug {
    /// Creates a new `PostSlug` with validation.
    /// Returns `ValidationErrors` if the input doesn't match the expected format.
    pub fn new(slug: String) -> Result<Self, validator::ValidationErrors> {
        let this = Self { slug };
        this.validate()?;
        Ok(this)
    }

    pub fn as_str(&self) -> &str {
        &self.slug
    }
}

// Internal use only – data must already be validated.
// For untrusted input, use `PostSlug::new()` which runs validation.
impl From<String> for PostSlug {
    fn from(slug: String) -> Self {
        Self { slug }
    }
}

impl From<&str> for PostSlug {
    fn from(s: &str) -> Self {
        Self {
            slug: s.to_string(),
        }
    }
}

/// A comment that has been projected into our read database.
/// This is the data structure that will be returned by the API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub event_id: String,
    pub site_id: String,
    pub post_slug: String,
    pub author: CommentAuthor,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    /// Matrix event ID of the parent comment, when this comment is a reply.
    pub reply_to: Option<String>,
    /// Matrix room the comment lives in. Internal integrity check for edits
    /// and redactions; never exposed through the API/SSE.
    #[serde(skip)]
    pub room_id: String,
    /// Matrix sender of the original event. Internal integrity check for
    /// edits (m.replace) and never exposed through the API/SSE.
    #[serde(skip)]
    pub sender_mxid: String,
}

/// Which identity model a comment belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthorType {
    /// Posted through the Cumments API by an AS virtual user; ownership is
    /// the Ed25519 public key embedded in the event.
    Guest,
    /// Posted directly in Matrix by a regular account; ownership is governed
    /// by Matrix (sender identity and room power levels).
    Matrix,
}

impl AuthorType {
    pub fn as_str(&self) -> &'static str {
        match self {
            AuthorType::Guest => "guest",
            AuthorType::Matrix => "matrix",
        }
    }

    /// Parse a stored `comments.author_type` value, falling back to the
    /// legacy signal (presence of a public key) for rows written before the
    /// column existed.
    pub fn from_db(value: &str, has_public_key: bool) -> Self {
        match value {
            "guest" => AuthorType::Guest,
            "matrix" => AuthorType::Matrix,
            _ if has_public_key => AuthorType::Guest,
            _ => AuthorType::Matrix,
        }
    }
}

/// Author identity exposed through the API.
///
/// - Guest comments carry `public_key`; `mxid` is intentionally not exposed
///   because the virtual user ID is an implementation detail derived from the
///   key and site.
/// - Matrix-native comments carry `mxid`; `public_key` is always `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentAuthor {
    #[serde(rename = "type")]
    pub kind: AuthorType,
    pub display_name: Option<String>,
    pub public_key: Option<String>,
    pub mxid: Option<String>,
}

/// Represents a website that uses Cumments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Site {
    pub id: String,
    pub matrix_space_id: String,
    pub display_name: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One page of room history fetched from the homeserver.
#[derive(Debug, Clone, Default)]
pub struct RoomEventPage {
    /// Raw Matrix room events (`m.room.message`, `m.room.redaction`, ...).
    pub events: Vec<serde_json::Value>,
    /// Token to continue fetching older history, if more is available.
    pub next_token: Option<String>,
    /// `true` when the homeserver reported more history in this direction.
    pub has_more: bool,
}

/// Identity of a Cumments room, extracted from metadata or alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomIdentity {
    pub site_id: String,
    pub post_slug: String,
}

/// One page of projected comments for a site/post.
#[derive(Debug, Clone, Default)]
pub struct CommentPage {
    pub items: Vec<Comment>,
    pub total: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_id_and_post_slug_accept_lowercase_hyphenated_slugs() {
        assert!(SiteId::new("my-blog".to_string()).is_ok());
        assert!(PostSlug::new("hello-world".to_string()).is_ok());
        assert!(SiteId::new("a1-b2".to_string()).is_ok());
    }

    #[test]
    fn site_id_and_post_slug_reject_underscores_and_uppercase() {
        assert!(SiteId::new("my_blog".to_string()).is_err());
        assert!(PostSlug::new("hello_world".to_string()).is_err());
        assert!(SiteId::new("My-Blog".to_string()).is_err());
        assert!(PostSlug::new("Hello-World".to_string()).is_err());
    }

    #[test]
    fn author_type_parses_db_values_with_legacy_fallback() {
        assert_eq!(AuthorType::from_db("guest", false), AuthorType::Guest);
        assert_eq!(AuthorType::from_db("matrix", true), AuthorType::Matrix);
        // Legacy rows written before the column existed: a stored public key
        // means guest, anything else is a Matrix-native comment.
        assert_eq!(AuthorType::from_db("", true), AuthorType::Guest);
        assert_eq!(AuthorType::from_db("", false), AuthorType::Matrix);
    }
}
