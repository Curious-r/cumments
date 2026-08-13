//! Site governance reconciliation: replicates the site Space's role roster
//! into every active comment room.
//!
//! Matrix power levels are the source of truth; this pass only makes Matrix
//! writes converge with the Space (add missing roles, remove revoked roles)
//! and never touches per-room moderators (level < 75).

use super::*;
use cumments_core::governance::{
    RoleEntry, SITE_ROLE_MIN_LEVEL, ensure_role_lock, reconcile_site_roles, role_entries,
    set_role_level,
};
use cumments_core::models::SiteId;
use tracing::warn;

impl Reconciler {
    /// Applies role claims that the target MXID verified through the DM
    /// token flow: reads the activated claims, writes the role into Matrix
    /// power levels, then marks the claim applied. Site-level roles provision
    /// the Space on first use.
    pub(super) async fn reconcile_claims(&self) -> Result<u64> {
        self.role_claim_store.purge_expired_claims().await?;
        let claims = self.role_claim_store.activated_unapplied_claims().await?;
        let mut applied = 0u64;
        for claim in claims {
            let room_id = if claim.room_id.is_empty() {
                let Ok(site_id) = SiteId::new(claim.site_id.clone()) else {
                    continue;
                };
                self.site_service
                    .ensure_space(&site_id, self.driver.as_ref())
                    .await?
            } else {
                claim.room_id.clone()
            };
            set_role_level(
                self.driver.as_ref(),
                &room_id,
                &claim.user_id,
                claim.level,
                true,
            )
            .await?;
            if let Err(e) = self.driver.invite_user(&room_id, &claim.user_id).await {
                warn!(
                    "claim apply: invite {} to {} failed: {:#}",
                    claim.user_id, room_id, e
                );
            }
            self.role_claim_store.mark_claim_applied(claim.id).await?;
            applied += 1;
        }
        Ok(applied)
    }

    /// Converges site-managed roles (owner 100 / co-manager 75) from each
    /// site Space into its active comment rooms. Returns the number of sites
    /// whose roles were inspected.
    pub(super) async fn reconcile_moderation(&self) -> Result<u64> {
        let Some(sender) = self.driver.sender_user_id() else {
            return Ok(0);
        };

        let sites = self.site_store.list_sites().await?;
        let mut inspected = 0u64;
        for site in sites {
            if site.matrix_space_id.is_empty() {
                continue;
            }
            let Some(space_power_levels) = self
                .driver
                .get_room_power_levels(&site.matrix_space_id)
                .await?
            else {
                continue;
            };

            // Normalize legacy Spaces that predate the governance lock.
            let locked = ensure_role_lock(&space_power_levels);
            if locked != space_power_levels {
                self.driver
                    .set_room_power_levels(&site.matrix_space_id, &locked)
                    .await?;
            }

            let site_roles: Vec<RoleEntry> = role_entries(&space_power_levels, SITE_ROLE_MIN_LEVEL)
                .into_iter()
                .filter(|role| role.user_id != sender)
                .collect();
            let Ok(site_id) = SiteId::new(site.id.clone()) else {
                continue;
            };

            for room_id in self
                .registry_store
                .list_active_rooms_for_site(&site_id)
                .await?
            {
                let Some(room_power_levels) = self.driver.get_room_power_levels(&room_id).await?
                else {
                    continue;
                };
                let target = reconcile_site_roles(&room_power_levels, &sender, &site_roles);
                if target != room_power_levels {
                    self.driver.set_room_power_levels(&room_id, &target).await?;
                }
                for role in &site_roles {
                    if let Err(e) = self.driver.invite_user(&room_id, &role.user_id).await {
                        warn!(
                            "moderation sync: invite {} to {} failed: {:#}",
                            role.user_id, room_id, e
                        );
                    }
                }
            }

            // Site roles belong to the Space itself as well.
            for role in &site_roles {
                if let Err(e) = self
                    .driver
                    .invite_user(&site.matrix_space_id, &role.user_id)
                    .await
                {
                    warn!(
                        "moderation sync: invite {} to space {} failed: {:#}",
                        role.user_id, site.matrix_space_id, e
                    );
                }
            }
            inspected += 1;
        }
        Ok(inspected)
    }
}
