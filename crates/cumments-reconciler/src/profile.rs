//! Matrix visitor profile observation and projection.
//!
//! Projects authoritative Matrix runtime profiles into deterministic Cumments
//! [`MediaReference`]s without creating a second profile authority or modifying
//! homeserver state.

use anyhow::Result;
use std::sync::Arc;

use cumments_core::media_reference::MediaReference;
use cumments_core::models::{SiteId, VisitorProfile};
use cumments_core::ports::{MatrixDriver, MediaReferenceStore, VirtualUserStore};

/// Background workflow for projecting authoritative Matrix visitor profiles into deterministic MediaReferences.
pub struct ExternalProfileReconciler {
    driver: Arc<dyn MatrixDriver>,
    media_store: Arc<dyn MediaReferenceStore>,
    virtual_user_store: Option<Arc<dyn VirtualUserStore>>,
}

impl ExternalProfileReconciler {
    pub fn new(driver: Arc<dyn MatrixDriver>, media_store: Arc<dyn MediaReferenceStore>) -> Self {
        Self {
            driver,
            media_store,
            virtual_user_store: None,
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

    /// Projects an authoritative Matrix visitor profile reading into a [`MediaReference`].
    ///
    /// Reads the homeserver profile via `driver.get_profile`; when it carries an
    /// avatar MXC, derives the deterministic `MediaReference` for `(site_id, mxc)`
    /// and materializes the lookup mapping.
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

    /// Projects an observed [`VisitorProfile`] into the deterministic [`MediaReference`] for its avatar.
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

        // Materialize the lookup mapping so runtime reverse lookup keeps working.
        self.media_store
            .get_or_create_reference(site_id, mxc)
            .await?;

        Ok(Some(reference))
    }

    /// Projects an authoritative Matrix profile reading for a virtual user into a [`MediaReference`].
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
