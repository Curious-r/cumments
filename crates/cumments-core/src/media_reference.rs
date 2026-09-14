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
    ///
    /// Canonicalizes the textual representation to lowercase hyphenated format,
    /// so case variants of the same UUID resolve to identical `MediaReference` values.
    pub fn parse(s: &str) -> Result<Self, MediaReferenceParseError> {
        let rest = s
            .strip_prefix(MEDIA_REFERENCE_PREFIX)
            .ok_or(MediaReferenceParseError::MissingPrefix)?;
        let uuid = Uuid::parse_str(rest)?;
        Ok(Self::new(uuid))
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

/// Background reconciliation service for externally assigned Matrix avatars.
///
/// Converts externally observed Matrix avatar MXC URIs (e.g. from room member
/// state or global Matrix profile state) into durable [`MediaReference`] identities.
///
/// Invariants:
/// - Allocates a stable [`MediaReference`] for previously unseen MXC URIs on this site,
///   marked `is_external = true`.
/// - Idempotently reuses an existing mapping without modifying its provenance.
/// - Never alters Matrix profile authority (read-only with respect to homeserver profile state).
/// - Never creates or modifies upload ownership records (`media_uploads`).
pub struct ExternalAvatarReconciler {
    store: std::sync::Arc<dyn crate::ports::MediaReferenceStore>,
}

impl ExternalAvatarReconciler {
    pub fn new(store: std::sync::Arc<dyn crate::ports::MediaReferenceStore>) -> Self {
        Self { store }
    }

    /// Reconciles an avatar MXC URI into a stable [`MediaReference`].
    ///
    /// If previously unseen, creates a new mapping marked `is_external = true`.
    /// If already known, reuses the existing mapping without modifying its provenance.
    pub async fn reconcile_avatar(
        &self,
        site_id: &crate::models::SiteId,
        mxc_uri: &str,
    ) -> anyhow::Result<MediaReference> {
        self.store
            .get_or_create_reference(site_id, mxc_uri, true)
            .await
    }

    /// Reconciles an avatar URL observed in an `m.room.member` state event.
    ///
    /// Returns `Ok(Some(reference))` if `avatar_url` is a valid `mxc://...` URI,
    /// or `Ok(None)` if no avatar is set.
    pub async fn reconcile_room_member_avatar(
        &self,
        site_id: &crate::models::SiteId,
        avatar_url: Option<&str>,
    ) -> anyhow::Result<Option<MediaReference>> {
        match avatar_url {
            Some(mxc) if mxc.starts_with("mxc://") => {
                let reference = self.reconcile_avatar(site_id, mxc).await?;
                Ok(Some(reference))
            }
            _ => Ok(None),
        }
    }

    /// Reconciles an avatar URL observed in a global Matrix profile.
    ///
    /// Returns `Ok(Some(reference))` if `avatar_url` is a valid `mxc://...` URI,
    /// or `Ok(None)` if no avatar is set.
    pub async fn reconcile_global_profile_avatar(
        &self,
        site_id: &crate::models::SiteId,
        avatar_url: Option<&str>,
    ) -> anyhow::Result<Option<MediaReference>> {
        match avatar_url {
            Some(mxc) if mxc.starts_with("mxc://") => {
                let reference = self.reconcile_avatar(site_id, mxc).await?;
                Ok(Some(reference))
            }
            _ => Ok(None),
        }
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
    fn uppercase_and_lowercase_inputs_canonicalize_identically() {
        let lower_input = "cumments-media:550e8400-e29b-41d4-a716-446655440000";
        let upper_input = "cumments-media:550E8400-E29B-41D4-A716-446655440000";
        let mixed_input = "cumments-media:550e8400-E29B-41d4-A716-446655440000";

        let lower_ref = MediaReference::parse(lower_input).expect("parse lower");
        let upper_ref = MediaReference::parse(upper_input).expect("parse upper");
        let mixed_ref = MediaReference::parse(mixed_input).expect("parse mixed");

        // Canonical equality
        assert_eq!(lower_ref, upper_ref);
        assert_eq!(upper_ref, mixed_ref);

        // Display and as_str always produce lowercase canonical form
        assert_eq!(lower_ref.as_str(), lower_input);
        assert_eq!(upper_ref.as_str(), lower_input);
        assert_eq!(mixed_ref.as_str(), lower_input);

        assert_eq!(lower_ref.to_string(), lower_input);
        assert_eq!(upper_ref.to_string(), lower_input);
        assert_eq!(mixed_ref.to_string(), lower_input);

        // Serialization always produces lowercase canonical form
        let json_from_upper = serde_json::to_string(&upper_ref).expect("serialize");
        assert_eq!(json_from_upper, format!("\"{lower_input}\""));

        // Deserialization canonicalizes uppercase
        let deserialized: MediaReference =
            serde_json::from_str(&format!("\"{upper_input}\"")).expect("deserialize");
        assert_eq!(deserialized, lower_ref);
        assert_eq!(deserialized.as_str(), lower_input);
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
