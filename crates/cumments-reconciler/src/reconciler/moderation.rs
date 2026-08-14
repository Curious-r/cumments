//! Site governance reconciliation: applies verified role claims and
//! replicates the site Space's role roster into every active comment room.
//!
//! Matrix power levels are the source of truth; these passes only make
//! Matrix writes converge with the Space (add missing roles, remove revoked
//! roles) and never touch per-room moderators (level < 75).

use super::*;
use anyhow::Result;
use async_trait::async_trait;
use cumments_core::governance::{
    RoleEntry, SITE_ROLE_MIN_LEVEL, ensure_role_lock, reconcile_site_roles, role_entries,
    set_role_level,
};
use cumments_core::models::SiteId;
use tracing::{info, warn};

/// Applies role claims that the target MXID verified through the DM token
/// flow, writing them into Matrix power levels.
pub struct ClaimsPass {
    deps: Arc<ReconcilerDeps>,
    config: PassConfig,
}

impl ClaimsPass {
    pub fn new(deps: Arc<ReconcilerDeps>, config: PassConfig) -> Self {
        Self { deps, config }
    }

    async fn reconcile(&self) -> Result<u64> {
        // Leave claim DMs whose claims expired before their rows are purged.
        self.leave_unneeded_claim_dms().await;
        self.deps.role_claim_store.purge_expired_claims().await?;
        let claims = self
            .deps
            .role_claim_store
            .activated_unapplied_claims()
            .await?;
        let mut applied = 0u64;
        for claim in claims {
            let room_id = if claim.room_id.is_empty() {
                let Ok(site_id) = SiteId::new(claim.site_id.clone()) else {
                    continue;
                };
                self.deps
                    .site_service
                    .ensure_space(&site_id, self.deps.driver.as_ref())
                    .await?
            } else {
                claim.room_id.clone()
            };
            set_role_level(
                self.deps.driver.as_ref(),
                &room_id,
                &claim.user_id,
                claim.level,
                true,
            )
            .await?;
            if let Err(e) = self.deps.driver.invite_user(&room_id, &claim.user_id).await {
                warn!(
                    "claim apply: invite {} to {} failed: {:#}",
                    claim.user_id, room_id, e
                );
            }
            self.deps
                .role_claim_store
                .mark_claim_applied(claim.id)
                .await?;
            applied += 1;
        }
        self.reconcile_applied_claims().await?;
        self.leave_unneeded_claim_dms().await;
        Ok(applied)
    }

    /// Converges applied claim rows with the projected Matrix roles: a row
    /// whose role no longer exists (removed via API or directly in Matrix)
    /// is marked revoked.
    async fn reconcile_applied_claims(&self) -> Result<()> {
        for claim in self.deps.role_claim_store.list_applied_claims().await? {
            let present = if claim.room_id.is_empty() {
                let Ok(site_id) = SiteId::new(claim.site_id.clone()) else {
                    continue;
                };
                self.deps
                    .governance_store
                    .list_site_roles(site_id.as_str())
                    .await?
                    .iter()
                    .any(|role| role.user_id == claim.user_id && role.level == claim.level)
            } else {
                self.deps
                    .governance_store
                    .list_room_roles(&claim.room_id)
                    .await?
                    .iter()
                    .any(|role| role.user_id == claim.user_id && role.level == claim.level)
            };
            if !present {
                self.deps
                    .role_claim_store
                    .mark_applied_claim_revoked(
                        &claim.site_id,
                        &claim.room_id,
                        &claim.user_id,
                        claim.level,
                    )
                    .await?;
            }
        }
        Ok(())
    }

    /// Best-effort exit from claim DMs once the user has no pending or
    /// activated claims left in them.
    async fn leave_unneeded_claim_dms(&self) {
        let Ok(rooms) = self.deps.role_claim_store.claim_dm_rooms().await else {
            return;
        };
        for (user_id, room_id) in rooms {
            let active = self
                .deps
                .role_claim_store
                .active_claims_in_dm_room(&user_id, &room_id)
                .await
                .unwrap_or(true);
            if active {
                continue;
            }
            match self.deps.driver.leave_room(&room_id).await {
                Ok(()) => info!("Bot left claim DM {room_id} for {user_id}"),
                Err(e) => warn!(
                    "Bot failed to leave claim DM {room_id} for {user_id}: {:#}",
                    e
                ),
            }
        }
    }
}

#[async_trait]
impl ReconcilePass for ClaimsPass {
    fn config(&self) -> &PassConfig {
        &self.config
    }

    async fn run(&self) -> Result<u64> {
        self.reconcile().await
    }
}

/// Converges site-managed roles (owner 100 / co-manager 75) from each site
/// Space into its active comment rooms.
pub struct ModerationPass {
    deps: Arc<ReconcilerDeps>,
    config: PassConfig,
}

impl ModerationPass {
    pub fn new(deps: Arc<ReconcilerDeps>, config: PassConfig) -> Self {
        Self { deps, config }
    }

    async fn reconcile(&self) -> Result<u64> {
        let Some(sender) = self.deps.driver.sender_user_id() else {
            return Ok(0);
        };

        let sites = self.deps.site_store.list_sites().await?;
        let mut inspected = 0u64;
        for site in sites {
            if site.matrix_space_id.is_empty() {
                continue;
            }
            let Some(space_power_levels) = self
                .deps
                .driver
                .get_room_power_levels(&site.matrix_space_id)
                .await?
            else {
                continue;
            };

            // Normalize legacy Spaces that predate the governance lock.
            let locked = ensure_role_lock(&space_power_levels);
            if locked != space_power_levels {
                self.deps
                    .driver
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
                .deps
                .registry_store
                .list_active_rooms_for_site(&site_id)
                .await?
            {
                let Some(room_power_levels) =
                    self.deps.driver.get_room_power_levels(&room_id).await?
                else {
                    continue;
                };
                let target = reconcile_site_roles(&room_power_levels, &sender, &site_roles);
                if target != room_power_levels {
                    self.deps
                        .driver
                        .set_room_power_levels(&room_id, &target)
                        .await?;
                }
                for role in &site_roles {
                    if let Err(e) = self.deps.driver.invite_user(&room_id, &role.user_id).await {
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
                    .deps
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

#[async_trait]
impl ReconcilePass for ModerationPass {
    fn config(&self) -> &PassConfig {
        &self.config
    }

    async fn run(&self) -> Result<u64> {
        self.reconcile().await
    }
}
