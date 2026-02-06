//! Defines the core data models of the application.
//! These models should be pure data structures with no logic tied to infrastructure.

use serde::{Deserialize, Serialize};

// A validated, owned representation of a Site ID.
// More validation logic will be added later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SiteId(String);

// A validated, owned representation of a Post Slug.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PostSlug(String);
