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
    pub author_nickname: Option<String>,
    /// Public Ed25519 key of the author (base64url); safe to expose.
    pub author_public_key: Option<String>,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    /// Matrix sender of the original event. Internal integrity check for
    /// edits (m.replace) and never exposed through the API/SSE.
    #[serde(skip)]
    pub author_mxid: String,
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
    pub next_batch: Option<String>,
    /// `true` when the homeserver reported the start/end boundary (no more
    /// history in this direction).
    pub done: bool,
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
}
