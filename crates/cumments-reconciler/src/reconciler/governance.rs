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
    RoleEntry, SITE_ADMIN_LEVEL, SITE_ROLE_MIN_LEVEL, ensure_space_governance_locks,
    reconcile_site_roles, role_entries, set_role_level,
};
use cumments_core::management::complete_owner_transfer;
use cumments_core::models::{RoomStateSnapshot, SiteId};
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
        self.deps
            .site_transfer_store
            .expire_pending_transfers()
            .await?;
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

            if claim.room_id.is_empty()
                && claim.level == SITE_ADMIN_LEVEL
                && let Some(dm_room_id) = claim.dm_room_id.clone()
            {
                match complete_owner_transfer(
                    self.deps.site_transfer_store.as_ref(),
                    self.deps.site_auth_store.as_ref(),
                    self.deps.driver.as_ref(),
                    &self.deps.site_service,
                    &claim.site_id,
                    &claim.user_id,
                    &dm_room_id,
                )
                .await
                {
                    Ok(true) => info!(
                        "owner transfer completed for site {} -> {}",
                        claim.site_id, claim.user_id
                    ),
                    Ok(false) => {}
                    Err(error) => warn!(
                        "owner transfer completion failed for site {} -> {}: {error:#}",
                        claim.site_id, claim.user_id
                    ),
                }
            }
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

/// Converges site-managed roles (admin 100 / manager 75) from each site
/// Space into its active comment rooms.
pub struct GovernanceSyncPass {
    deps: Arc<ReconcilerDeps>,
    config: PassConfig,
}

impl GovernanceSyncPass {
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
            capture_room_state_snapshot(self.deps.as_ref(), &site.matrix_space_id).await?;
            let Some(space_power_levels) = self
                .deps
                .driver
                .get_room_power_levels(&site.matrix_space_id)
                .await?
            else {
                continue;
            };

            // Normalize legacy Spaces that predate the governance lock.
            let locked = ensure_space_governance_locks(&space_power_levels);
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
                capture_room_state_snapshot(self.deps.as_ref(), &room_id).await?;
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
                            "governance sync: invite {} to {} failed: {:#}",
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
                        "governance sync: invite {} to space {} failed: {:#}",
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
impl ReconcilePass for GovernanceSyncPass {
    fn config(&self) -> &PassConfig {
        &self.config
    }

    async fn run(&self) -> Result<u64> {
        self.reconcile().await
    }
}

/// Replaces the local snapshot with the homeserver's resolved current state.
///
/// Historical events in `room_state_events` are useful for audit and replay,
/// but redaction rules and governance decisions must use what the homeserver
/// currently resolves—not a latest-wins reconstruction of a timeline.
async fn capture_room_state_snapshot(deps: &ReconcilerDeps, room_id: &str) -> Result<()> {
    let create_content = deps
        .driver
        .get_room_state(room_id, "m.room.create", "")
        .await?;
    let power_levels = deps.driver.get_room_power_levels(room_id).await?;
    deps.room_store
        .save_room_state_snapshot(&RoomStateSnapshot {
            room_id: room_id.to_string(),
            room_version: create_content
                .as_ref()
                .and_then(|content| content.get("room_version"))
                .and_then(|version| version.as_str())
                .map(str::to_string),
            create_content_json: create_content,
            power_levels_json: power_levels,
            resolved_at: chrono::Utc::now(),
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cumments_core::{
        governance::{NewRoleClaim, RoleEntry, SITE_ADMIN_LEVEL},
        ports::{RoleClaimStore, RoomStore, SiteAuthStore, SiteTransferStore},
        site_auth::token_hash,
        site_service::SiteService,
    };
    use cumments_store::DbStore;
    use cumments_test_utils::TestDriver;
    use std::sync::Arc;
    use tokio::sync::Notify;

    fn test_db_url(name: &str) -> String {
        let path = std::path::Path::new("/tmp").join(format!(
            "cumments-governance-test-{}-{}.db",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        std::fs::File::create(&path).expect("create db file");
        format!("sqlite://{}", path.display())
    }

    #[tokio::test]
    async fn claims_pass_completes_transfer_and_resets_admins() {
        let store = Arc::new(
            DbStore::connect(&test_db_url("transfer"))
                .await
                .expect("connect db"),
        );
        let site_id = "transfer-site";
        store
            .register_site(site_id, &token_hash("old-token"), false)
            .await
            .expect("register site");
        store
            .ensure_site_exists(site_id, "!space:hs")
            .await
            .expect("attach space");

        store
            .upsert_role_claim(&NewRoleClaim {
                site_id: site_id.to_string(),
                room_id: String::new(),
                user_id: "@new-admin:hs".to_string(),
                level: SITE_ADMIN_LEVEL,
                token_hash: "verify-hash".to_string(),
                expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            })
            .await
            .expect("claim");
        let claim = store
            .pending_claims_for_user("@new-admin:hs")
            .await
            .expect("pending")
            .remove(0);
        store
            .set_claim_dm_room_for_user("@new-admin:hs", "!dm:hs")
            .await
            .expect("dm room");
        assert!(store.mark_claim_activated(claim.id).await.unwrap());
        store
            .upsert_pending_transfer(
                site_id,
                "@new-admin:hs",
                chrono::Utc::now() + chrono::Duration::hours(1),
            )
            .await
            .expect("transfer");
        store
            .replace_site_roles(
                site_id,
                &[
                    RoleEntry {
                        user_id: "@old-admin:hs".into(),
                        level: SITE_ADMIN_LEVEL,
                    },
                    RoleEntry {
                        user_id: "@new-admin:hs".into(),
                        level: SITE_ADMIN_LEVEL,
                    },
                ],
            )
            .await
            .expect("project roles");

        let driver = Arc::new(TestDriver::new().with_power_levels(
            "!space:hs",
            serde_json::json!({
                "users": {
                    "@old-admin:hs": SITE_ADMIN_LEVEL,
                    "@manager:hs": 75,
                },
                "events": {
                    "m.room.power_levels": 100,
                    "m.room.tombstone": 150,
                },
                "state_default": 50,
            }),
        ));
        let deps = Arc::new(ReconcilerDeps {
            submission_store: store.clone(),
            registry_store: store.clone(),
            site_store: store.clone(),
            role_claim_store: store.clone(),
            governance_store: store.clone(),
            projection_repair_store: store.clone(),
            message_store: store.clone(),
            room_store: store.clone(),
            virtual_user_store: store.clone(),
            site_auth_store: store.clone(),
            site_transfer_store: store.clone(),
            state_redaction_repairer: driver.clone(),
            driver: driver.clone(),
            site_service: Arc::new(SiteService::new(
                store.clone() as Arc<dyn cumments_core::ports::SiteStore>
            )),
        });
        let pass = ClaimsPass::new(
            deps,
            PassConfig {
                name: "claims-transfer-test",
                interval: std::time::Duration::from_secs(60),
                wakeup: Arc::new(Notify::new()),
            },
        );
        assert_eq!(pass.run().await.expect("claims pass"), 1);

        let pl = driver
            .power_levels
            .lock()
            .await
            .get("!space:hs")
            .cloned()
            .expect("space power levels");
        assert_eq!(pl["users"]["@new-admin:hs"], SITE_ADMIN_LEVEL);
        assert!(pl["users"].get("@old-admin:hs").is_none());
        assert_eq!(pl["users"]["@manager:hs"], 75);
        assert!(
            store
                .find_pending_transfer(site_id)
                .await
                .unwrap()
                .is_none()
        );
        let auth = store
            .get_site_auth(site_id)
            .await
            .expect("site auth")
            .expect("exists");
        let new_hash = auth.claim_token_hash.expect("rotated token hash");
        assert_ne!(new_hash, token_hash("old-token"));
        let replies = driver.replies.lock().await;
        assert!(
            replies
                .iter()
                .any(|(room, body)| room == "!dm:hs" && body.contains("New claim token"))
        );
    }

    #[tokio::test]
    async fn governance_sync_captures_homeserver_state_snapshots() {
        let store = Arc::new(
            DbStore::connect(&test_db_url("state-snapshots"))
                .await
                .expect("connect db"),
        );
        store
            .register_site("snapshot-site", &token_hash("claim"), true)
            .await
            .expect("register site");
        store
            .ensure_site_exists("snapshot-site", "!space:hs")
            .await
            .expect("attach space");
        store
            .register_room(
                "!room:hs",
                &SiteId::from("snapshot-site"),
                &PageSlug::from("hello"),
            )
            .await
            .expect("register room");

        let create = serde_json::json!({ "room_version": "12" });
        let power_levels = serde_json::json!({
            "users": { "@_cumments_bot:hs": 150 },
            "events": { "m.room.power_levels": 100, "m.room.tombstone": 150 },
            "state_default": 50,
        });
        let driver = Arc::new(
            TestDriver::new()
                .with_power_levels("!space:hs", power_levels.clone())
                .with_power_levels("!room:hs", power_levels.clone())
                .with_room_state("!space:hs", "m.room.create", "", create.clone())
                .with_room_state("!room:hs", "m.room.create", "", create),
        );
        let deps = Arc::new(ReconcilerDeps {
            submission_store: store.clone(),
            registry_store: store.clone(),
            site_store: store.clone(),
            role_claim_store: store.clone(),
            governance_store: store.clone(),
            projection_repair_store: store.clone(),
            message_store: store.clone(),
            room_store: store.clone(),
            virtual_user_store: store.clone(),
            site_auth_store: store.clone(),
            site_transfer_store: store.clone(),
            state_redaction_repairer: driver.clone(),
            driver: driver.clone(),
            site_service: Arc::new(SiteService::new(
                store.clone() as Arc<dyn cumments_core::ports::SiteStore>
            )),
        });
        let pass = GovernanceSyncPass::new(
            deps,
            PassConfig {
                name: "state-snapshot-test",
                interval: std::time::Duration::from_secs(60),
                wakeup: Arc::new(Notify::new()),
            },
        );
        assert_eq!(pass.run().await.expect("governance sync"), 1);

        for room_id in ["!space:hs", "!room:hs"] {
            let snapshot = store
                .get_room_state_snapshot(room_id)
                .await
                .expect("get snapshot")
                .unwrap_or_else(|| panic!("missing snapshot for {room_id}"));
            assert_eq!(snapshot.room_version.as_deref(), Some("12"));
            assert_eq!(snapshot.power_levels_json, Some(power_levels.clone()));
        }
    }
}
