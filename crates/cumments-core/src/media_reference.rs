//! Stable Cumments media-domain identity.
//!
//! An opaque identifier representing an uploaded or reconciled media asset.
//! Canonical form: `cumments-media:<uuid>` (e.g. `cumments-media:550e8400-e29b-41d4-a716-446655440000`).
//!
//! `MediaReference` encapsulates the media-domain identity without coupling to
//! underlying Matrix homeserver storage details such as `mxc://...` URIs.
//!
//! A reference is derived deterministically from the site-scoped Matrix media
//! identity `(site_id, mxc_uri)`; see [`MediaReference::from_media`].

use std::fmt;
use std::ops::Deref;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::models::SiteId;

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

/// Length-prefix one field into the digest.
///
/// Each field is preceded by its big-endian `u64` byte length, the same
/// unambiguous framing used by
/// [`crate::submissions::deterministic_transaction_id`]. Without the prefix the
/// pair `("a", "bc")` and `("ab", "c")` would hash identically.
fn update_length_prefixed(hasher: &mut Sha256, field: &str) {
    hasher.update((field.len() as u64).to_be_bytes());
    hasher.update(field.as_bytes());
}

impl MediaReference {
    /// Wrap an existing UUID in the canonical representation.
    ///
    /// Internal: a reference must either be derived from a media identity via
    /// [`MediaReference::from_media`] or parsed from its canonical form, so
    /// callers cannot mint an arbitrary identity from an arbitrary UUID.
    fn new(uuid: Uuid) -> Self {
        Self {
            uuid,
            canonical: format!("{MEDIA_REFERENCE_PREFIX}{uuid}"),
        }
    }

    /// Derive the deterministic `MediaReference` for a site-scoped Matrix media
    /// identity.
    ///
    /// The identifier is UUID v8 over the first 128 bits of
    /// `SHA-256(len(site_id) || site_id || len(mxc_uri) || mxc_uri)`, so the same
    /// `(site_id, mxc_uri)` always yields the same reference on every run and
    /// platform, while a different site yields a different reference for the
    /// same MXC. The UUID version and variant bits are set by [`Uuid::new_v8`].
    pub fn from_media(site_id: &SiteId, mxc_uri: &str) -> Self {
        let mut hasher = Sha256::new();
        update_length_prefixed(&mut hasher, site_id.as_str());
        update_length_prefixed(&mut hasher, mxc_uri);
        let digest = hasher.finalize();

        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        Self::new(Uuid::new_v8(bytes))
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

/// Provenance / discovery source of a [`MediaReference`] mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaReferenceSource {
    /// Created or owned by Cumments (e.g. visitor upload or profile mutation).
    Cumments,
    /// Discovered from an external Matrix media object.
    External,
}

impl MediaReferenceSource {
    pub fn is_external(&self) -> bool {
        matches!(self, Self::External)
    }
}

impl From<bool> for MediaReferenceSource {
    fn from(is_external: bool) -> Self {
        if is_external {
            Self::External
        } else {
            Self::Cumments
        }
    }
}

/// Background reconciliation service for externally assigned Matrix avatars.
///
/// Converts externally observed Matrix avatar MXC URIs (e.g. from explicit
/// external profile observation) into durable [`MediaReference`] identities.
///
/// Invariants:
/// - Allocates a stable [`MediaReference`] for previously unseen MXC URIs on this site,
///   marked `is_external = true` (`MediaReferenceSource::External`).
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

    /// Explicit external avatar discovery entrypoint.
    ///
    /// Reconciles an externally discovered Matrix avatar MXC URI into a stable [`MediaReference`].
    /// - If previously unseen, creates a new mapping marked `MediaReferenceSource::External` (`is_external = true`).
    /// - If already known, reuses the existing mapping without modifying its provenance.
    pub async fn reconcile_external_avatar(
        &self,
        site_id: &crate::models::SiteId,
        mxc_uri: &str,
    ) -> anyhow::Result<MediaReference> {
        self.store
            .get_or_create_reference(site_id, mxc_uri, MediaReferenceSource::External)
            .await
    }

    /// Explicit external profile avatar reconciliation entrypoint.
    ///
    /// Reconciles an avatar MXC observed from an external Matrix profile into a durable [`MediaReference`].
    /// - If previously unseen, creates a new mapping marked `MediaReferenceSource::External` (`is_external = true`).
    /// - If already known, reuses the existing mapping without modifying its provenance.
    pub async fn reconcile_external_profile_avatar(
        &self,
        site_id: &crate::models::SiteId,
        mxc_uri: &str,
    ) -> anyhow::Result<MediaReference> {
        self.reconcile_external_avatar(site_id, mxc_uri).await
    }

    /// Alias for [`Self::reconcile_external_avatar`].
    pub async fn reconcile_avatar(
        &self,
        site_id: &crate::models::SiteId,
        mxc_uri: &str,
    ) -> anyhow::Result<MediaReference> {
        self.reconcile_external_avatar(site_id, mxc_uri).await
    }

    /// Reconciles an avatar URL observed in an explicit external profile observation (e.g. global Matrix profile).
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
                let reference = self.reconcile_external_avatar(site_id, mxc).await?;
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
        let reference = MediaReference::from_media(&SiteId::from("blog"), "mxc://hs/roundtrip");
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

    #[test]
    fn from_media_is_deterministic_for_the_same_site_and_mxc() {
        let site = SiteId::from("site-a");
        let first = MediaReference::from_media(&site, "mxc://hs/asset");
        let second = MediaReference::from_media(&site, "mxc://hs/asset");
        assert_eq!(first, second);
        assert_eq!(first.as_str(), second.as_str());

        // An equal site id constructed independently yields the same identity.
        let other_site = SiteId::from("site-a");
        assert_eq!(
            first,
            MediaReference::from_media(&other_site, "mxc://hs/asset")
        );
    }

    #[test]
    fn from_media_separates_sites_for_the_same_mxc() {
        let mxc = "mxc://hs/shared";
        let site_a = MediaReference::from_media(&SiteId::from("site-a"), mxc);
        let site_b = MediaReference::from_media(&SiteId::from("site-b"), mxc);
        assert_ne!(site_a, site_b);
    }

    #[test]
    fn from_media_separates_mxcs_on_the_same_site() {
        let site = SiteId::from("site-a");
        let first = MediaReference::from_media(&site, "mxc://hs/one");
        let second = MediaReference::from_media(&site, "mxc://hs/two");
        assert_ne!(first, second);
    }

    #[test]
    fn from_media_produces_uuid_v8_with_rfc_variant() {
        let reference = MediaReference::from_media(&SiteId::from("site-a"), "mxc://hs/asset");
        assert_eq!(reference.uuid().get_version_num(), 8, "must be UUID v8");
        assert_eq!(
            reference.uuid().get_variant(),
            uuid::Variant::RFC4122,
            "must use the RFC 4122 variant"
        );
    }

    #[test]
    fn from_media_serializes_and_reparses_identically() {
        let reference = MediaReference::from_media(&SiteId::from("site-a"), "mxc://hs/asset");
        let canonical = reference.as_str();
        assert!(canonical.starts_with(MEDIA_REFERENCE_PREFIX));

        let json = serde_json::to_string(&reference).expect("serialize");
        assert_eq!(json, format!("\"{canonical}\""));

        let parsed = MediaReference::parse(canonical).expect("parse canonical");
        assert_eq!(parsed, reference);
        let deserialized: MediaReference = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized, reference);
    }

    #[test]
    fn from_media_length_prefixing_is_unambiguous() {
        // Naive delimiter-free concatenation would collide: "a" + "bc" == "ab" + "c".
        let left = MediaReference::from_media(&SiteId::from("a"), "bc");
        let right = MediaReference::from_media(&SiteId::from("ab"), "c");
        assert_ne!(left, right);
    }

    #[test]
    fn from_media_matches_the_fixed_reference_vector() {
        // Locks the derivation: changing the encoding, hash, or UUID derivation
        // for `(site-a, mxc://hs/asset)` must fail this test.
        let reference = MediaReference::from_media(&SiteId::from("site-a"), "mxc://hs/asset");
        assert_eq!(
            reference.as_str(),
            "cumments-media:e8b56568-c68c-8e48-8355-c563e07a9c00"
        );
    }

    #[test]
    fn parsing_recovers_the_deterministic_reference() {
        // Parsing remains the only way to reconstruct a reference from its
        // canonical form now that arbitrary UUID wrapping is internal.
        let derived = MediaReference::from_media(&SiteId::from("site-a"), "mxc://hs/asset");
        let parsed = MediaReference::parse("cumments-media:e8b56568-c68c-8e48-8355-c563e07a9c00")
            .expect("parse canonical reference");
        assert_eq!(parsed, derived);
        assert_eq!(parsed.uuid(), derived.uuid());
    }
}
