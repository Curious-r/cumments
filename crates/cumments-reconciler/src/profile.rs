//! External visitor profile observation and reconciliation.
//!
//! Reconciles authoritative Matrix runtime profiles into durable Cumments [`MediaReference`]s
//! without creating a second profile authority or modifying homeserver state.

use anyhow::Result;
use std::sync::Arc;

use cumments_core::media_reference::{
    ExternalAvatarReconciler, MediaReference, MediaReferenceSource,
};
use cumments_core::models::{SiteId, VisitorProfile};
use cumments_core::ports::{MatrixDriver, MediaReferenceStore, MessageStore};

/// Background workflow for reconciling authoritative Matrix visitor profiles into durable MediaReferences.
pub struct ExternalProfileReconciler {
    driver: Arc<dyn MatrixDriver>,
    media_store: Arc<dyn MediaReferenceStore>,
    message_store: Arc<dyn MessageStore>,
    avatar_reconciler: ExternalAvatarReconciler,
}

impl ExternalProfileReconciler {
    pub fn new(
        driver: Arc<dyn MatrixDriver>,
        media_store: Arc<dyn MediaReferenceStore>,
        message_store: Arc<dyn MessageStore>,
    ) -> Self {
        let avatar_reconciler = ExternalAvatarReconciler::new(media_store.clone());
        Self {
            driver,
            media_store,
            message_store,
            avatar_reconciler,
        }
    }

    /// Reconciles an authoritative Matrix visitor profile reading into a durable [`MediaReference`].
    ///
    /// 1. Queries the homeserver via `driver.get_profile(author_public_key, site_id)`
    ///    (`GET /_matrix/client/v3/profile/{userId}`).
    /// 2. If an avatar MXC URI is present:
    ///    - If an existing mapping exists for `(site_id, mxc)`, reuses it and preserves its provenance.
    ///    - If no mapping exists:
    ///      - If `media_uploads` contains this MXC for the site, records `MediaReferenceSource::Cumments` (`is_external = false`).
    ///      - Otherwise, records `MediaReferenceSource::External` (`is_external = true`).
    /// 3. Returns the resolved or newly allocated `MediaReference`, or `Ok(None)` if no avatar is set.
    pub async fn reconcile_visitor_profile(
        &self,
        site_id: &SiteId,
        author_public_key: &str,
    ) -> Result<Option<MediaReference>> {
        let profile = self.driver.get_profile(author_public_key, site_id).await?;
        let Some(profile) = profile else {
            return Ok(None);
        };
        self.reconcile_observed_profile(site_id, &profile).await
    }

    /// Reconciles an observed [`VisitorProfile`] from an authoritative Matrix read into a durable [`MediaReference`].
    pub async fn reconcile_observed_profile(
        &self,
        site_id: &SiteId,
        profile: &VisitorProfile,
    ) -> Result<Option<MediaReference>> {
        let Some(ref mxc) = profile.avatar_url else {
            return Ok(None);
        };
        if !mxc.starts_with("mxc://") {
            return Ok(None);
        }

        // Fast path: if mapping already exists, preserve its existing provenance
        if let Some(existing) = self.media_store.find_reference(site_id, mxc).await? {
            return Ok(Some(existing));
        }

        // Authoritative local check: did Cumments upload this media for this site?
        let source = if self
            .message_store
            .has_media_upload_for_site(site_id.as_str(), mxc)
            .await?
        {
            MediaReferenceSource::Cumments
        } else {
            // Authoritative external observation from Matrix global profile
            MediaReferenceSource::External
        };

        let media_ref = self
            .media_store
            .get_or_create_reference(site_id, mxc, source)
            .await?;
        Ok(Some(media_ref))
    }

    /// Explicit external profile avatar reconciliation entrypoint.
    ///
    /// Directly establishes an external mapping for an MXC URI observed from an external Matrix profile.
    pub async fn reconcile_external_profile_avatar(
        &self,
        site_id: &SiteId,
        mxc_uri: &str,
    ) -> Result<MediaReference> {
        self.avatar_reconciler
            .reconcile_external_profile_avatar(site_id, mxc_uri)
            .await
    }
}
