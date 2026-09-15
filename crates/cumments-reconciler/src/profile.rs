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
use cumments_core::ports::{MatrixDriver, MediaReferenceStore, MessageStore, VirtualUserStore};

/// Background workflow for reconciling authoritative Matrix visitor profiles into durable MediaReferences.
pub struct ExternalProfileReconciler {
    driver: Arc<dyn MatrixDriver>,
    media_store: Arc<dyn MediaReferenceStore>,
    message_store: Arc<dyn MessageStore>,
    virtual_user_store: Option<Arc<dyn VirtualUserStore>>,
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
            virtual_user_store: None,
            avatar_reconciler,
        }
    }

    /// Attaches an optional [`VirtualUserStore`] to enable resolving virtual user IDs.
    pub fn with_virtual_user_store(
        mut self,
        virtual_user_store: Arc<dyn VirtualUserStore>,
    ) -> Self {
        self.virtual_user_store = Some(virtual_user_store);
        self
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

    /// Projects an observed [`VisitorProfile`] from an authoritative Matrix
    /// read into the deterministic [`MediaReference`] for its avatar.
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

        // Matrix-derived projection: the identity is a pure function of the
        // observed `(site_id, avatar mxc)`. A missing `media_references` row
        // must not prevent representing the profile avatar.
        let reference = MediaReference::from_media(site_id, mxc);

        // Materialize the lookup/provenance row so runtime reverse lookup keeps
        // working. It is not the source of the identity derived above.
        let source = if self
            .message_store
            .has_media_upload_for_site(site_id.as_str(), mxc)
            .await?
        {
            MediaReferenceSource::Cumments
        } else {
            // Authoritative external observation from the Matrix global profile
            MediaReferenceSource::External
        };
        self.media_store
            .get_or_create_reference(site_id, mxc, source)
            .await?;

        Ok(Some(reference))
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

    /// Reconciles an authoritative Matrix profile reading for a virtual user into a durable [`MediaReference`].
    ///
    /// Looks up the author's public key from the virtual user store, then delegates to
    /// [`reconcile_visitor_profile`](Self::reconcile_visitor_profile).
    pub async fn reconcile_virtual_user_profile(
        &self,
        site_id: &SiteId,
        virtual_user_id: &str,
    ) -> Result<Option<MediaReference>> {
        let Some(virtual_user_store) = &self.virtual_user_store else {
            return Ok(None);
        };
        let Some(author_public_key) = virtual_user_store
            .find_author_public_key(virtual_user_id, site_id)
            .await?
        else {
            return Ok(None);
        };
        self.reconcile_visitor_profile(site_id, &author_public_key)
            .await
    }
}
