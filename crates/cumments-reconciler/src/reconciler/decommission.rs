//! Site decommission pass: retires Matrix rooms one by one and clears local
//! projections only after the Matrix side is gone.
//!
//! The order matters: rooms first, then the Space, then local cleanup. Once
//! the AS sender has left a room, `backfill` can no longer discover it, so
//! deleting its local rows cannot resurrect anything.

use super::*;
use anyhow::Result;
use tracing::warn;

impl Reconciler {
    /// Retires every site marked `retiring`. Returns how many finished.
    pub(super) async fn reconcile_decommissions(&self) -> Result<u64> {
        let retiring = self.site_auth_store.list_retiring_sites().await?;
        let mut finished = 0u64;
        for site_id in retiring {
            if let Err(error) = self.decommission_site(&site_id).await {
                warn!(site_id, "site decommission failed: {:#}", error);
                continue;
            }
            finished += 1;
        }
        Ok(finished)
    }

    async fn decommission_site(&self, raw_site_id: &str) -> Result<()> {
        let site_id = SiteId::new(raw_site_id.to_string())
            .map_err(|e| anyhow::anyhow!("invalid site id: {e}"))?;

        // Retire every comment room first, then the Space. Every operation
        // is idempotent, so a failed pass can simply be retried.
        for room_id in self
            .registry_store
            .list_active_rooms_for_site(&site_id)
            .await?
        {
            let identity = self
                .registry_store
                .get_registered_room_identity(&room_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("room {room_id} has no registry identity"))?;
            let post_slug = PostSlug::from(identity.post_slug.clone());
            self.driver
                .set_room_name(
                    &room_id,
                    &format!("[retired] {}/{}", raw_site_id, post_slug.as_str()),
                )
                .await?;
            self.driver
                .remove_room_alias(&site_id, Some(&post_slug))
                .await?;
            self.driver.leave_room(&room_id).await?;
        }

        let space_id = self
            .site_store
            .get_site(&site_id)
            .await?
            .map(|site| site.matrix_space_id)
            .unwrap_or_default();
        if !space_id.is_empty() {
            self.driver
                .set_room_name(&space_id, &format!("[retired] {raw_site_id}"))
                .await?;
            self.driver.remove_room_alias(&site_id, None).await?;
            self.driver.leave_room(&space_id).await?;
        }

        // Matrix side is fully retired: now clearing local rows cannot be
        // undone by a later backfill.
        self.site_auth_store.delete_site(raw_site_id).await?;
        Ok(())
    }
}
