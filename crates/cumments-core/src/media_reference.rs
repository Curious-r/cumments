//! Stable Cumments media-domain identity.
//!
//! An opaque identifier representing an uploaded or reconciled media asset.
//! Canonical form: `cumments-media:<uuid>` (e.g. `cumments-media:550e8400-e29b-41d4-a716-446655440000`).
//!
//! `MediaReference` encapsulates the media-domain identity without coupling to
//! underlying Matrix homeserver storage details such as `mxc://...` URIs.

use std::fmt;
use std::ops::Deref;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

/// Prefix required for the canonical textual representation of a `MediaReference`.
pub const MEDIA_REFERENCE_PREFIX: &str = "cumments-media:";

/// Parsing error for `MediaReference`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MediaReferenceParseError {
    #[error("missing 'cumments-media:' prefix")]
    MissingPrefix,
    #[error("invalid UUID component: {0}")]
    InvalidUuid(#[from] uuid::Error),
}

/// An opaque, stable Cumments media-domain identity.
///
/// Format: `cumments-media:<uuid>`
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MediaReference {
    uuid: Uuid,
    canonical: String,
}

impl MediaReference {
    /// Create a new `MediaReference` from an existing UUID.
    pub fn new(uuid: Uuid) -> Self {
        Self {
            uuid,
            canonical: format!("{MEDIA_REFERENCE_PREFIX}{uuid}"),
        }
    }

    /// Generate a fresh random v4 `MediaReference`.
    pub fn new_v4() -> Self {
        Self::new(Uuid::new_v4())
    }

    /// Parse a `MediaReference` from a string slice, validating prefix and UUID format.
    pub fn parse(s: &str) -> Result<Self, MediaReferenceParseError> {
        let rest = s
            .strip_prefix(MEDIA_REFERENCE_PREFIX)
            .ok_or(MediaReferenceParseError::MissingPrefix)?;
        let uuid = Uuid::parse_str(rest)?;
        Ok(Self {
            uuid,
            canonical: s.to_owned(),
        })
    }

    /// Returns the underlying UUID component.
    pub fn uuid(&self) -> Uuid {
        self.uuid
    }

    /// Returns the canonical string representation as a string slice.
    pub fn as_str(&self) -> &str {
        &self.canonical
    }
}

impl Deref for MediaReference {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.canonical
    }
}

impl AsRef<str> for MediaReference {
    fn as_ref(&self) -> &str {
        &self.canonical
    }
}

impl fmt::Display for MediaReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.canonical)
    }
}

impl fmt::Debug for MediaReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MediaReference({:?})", self.canonical)
    }
}

impl FromStr for MediaReference {
    type Err = MediaReferenceParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl From<Uuid> for MediaReference {
    fn from(uuid: Uuid) -> Self {
        Self::new(uuid)
    }
}

impl Serialize for MediaReference {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.canonical)
    }
}

impl<'de> Deserialize<'de> for MediaReference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_parsing_and_canonical_formatting() {
        let id_str = "550e8400-e29b-41d4-a716-446655440000";
        let full = format!("cumments-media:{id_str}");
        let parsed = MediaReference::parse(&full).expect("valid parse");
        assert_eq!(parsed.as_str(), full);
        assert_eq!(parsed.to_string(), full);
        assert_eq!(parsed.uuid(), Uuid::parse_str(id_str).unwrap());
        assert_eq!(parsed.as_ref(), full.as_str());
        assert_eq!(&*parsed, full.as_str());
    }

    #[test]
    fn parsing_rejects_missing_prefix() {
        let raw_uuid = "550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(
            MediaReference::parse(raw_uuid).unwrap_err(),
            MediaReferenceParseError::MissingPrefix
        );
    }

    #[test]
    fn parsing_rejects_invalid_uuid() {
        assert!(matches!(
            MediaReference::parse("cumments-media:not-a-uuid"),
            Err(MediaReferenceParseError::InvalidUuid(_))
        ));
    }

    #[test]
    fn parsing_rejects_empty_and_extra_content() {
        assert_eq!(
            MediaReference::parse("").unwrap_err(),
            MediaReferenceParseError::MissingPrefix
        );
        assert!(matches!(
            MediaReference::parse("cumments-media:"),
            Err(MediaReferenceParseError::InvalidUuid(_))
        ));
    }

    #[test]
    fn serde_json_roundtrip() {
        let reference = MediaReference::new_v4();
        let serialized = serde_json::to_string(&reference).expect("serialize");
        let expected_json = format!("\"{}\"", reference.as_str());
        assert_eq!(serialized, expected_json);

        let deserialized: MediaReference = serde_json::from_str(&serialized).expect("deserialize");
        assert_eq!(deserialized, reference);
    }

    #[test]
    fn serde_json_rejects_invalid() {
        assert!(serde_json::from_str::<MediaReference>("\"invalid\"").is_err());
        assert!(serde_json::from_str::<MediaReference>("\"cumments-media:invalid\"").is_err());
    }
}
