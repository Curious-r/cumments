//! Defines the core data models of the application.
//! These models should be pure data structures with no logic tied to infrastructure.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use validator::Validate;

lazy_static::lazy_static! {
    pub static ref ID_REGEX: regex::Regex = regex::Regex::new(r"^[a-zA-Z0-9_-]+$").unwrap();
}

// A validated, owned representation of a Site ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Validate)]
#[serde(transparent)]
pub struct SiteId {
    #[validate(regex(path = "*crate::models::ID_REGEX"))]
    pub id: String,
}

impl SiteId {
    pub fn as_str(&self) -> &str {
        &self.id
    }
}

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
    pub fn as_str(&self) -> &str {
        &self.slug
    }
}

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
    pub author_fingerprint: Option<String>,
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

/// Represents a website that uses Cumments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Site {
    pub id: String,
    pub matrix_space_id: String,
    pub display_name: Option<String>,
    pub created_at: DateTime<Utc>,
}
