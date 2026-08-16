//! Site decommission pass: retires Matrix rooms one by one and clears local
//! projections only after the Matrix side is gone.
//!
//! The order matters: rooms first, then the Space, then local cleanup. Once
//! the AS sender has left a room, `backfill` can no longer discover it, so
//! deleting its local rows cannot resurrect anything.

use super::*;
use anyhow::Result;
use async_trait::async_trait;
use tracing::warn;

/// Retires every site marked `retiring`.
pub struct DecommissionPass {
    deps: Arc<ReconcilerDeps>,
    config: PassConfig,
}

impl DecommissionPass {
    pub fn new(deps: Arc<ReconcilerDeps>, config: PassConfig) -> Self {
        Self { deps, config }
    }

    async fn reconcile(&self) -> Result<u64> {
        let retiring = self.deps.site_auth_store.list_retiring_sites().await?;
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

        // Collect everything the Matrix/leave steps depend on before the
        // local rows are deleted: all rooms (not just active ones), every
        // virtual user that must leave with the site, claim-DM rooms and
        // media URLs.
        let rooms = self
            .deps
            .registry_store
            .list_rooms_for_site(&site_id)
            .await?;
        let virtual_users = self
            .deps
            .virtual_user_store
            .list_virtual_users_for_site(&site_id)
            .await?;
        let media_urls = self
            .deps
            .message_store
            .list_media_urls_for_site(raw_site_id)
            .await?;
        let claim_dms = self
            .deps
            .role_claim_store
            .claim_dm_rooms_for_site(raw_site_id)
            .await?;

        // Retire every comment room first, then the Space. Every operation
        // is idempotent, so a failed pass can simply be retried.
        for room_id in &rooms {
            let identity = self
                .deps
                .registry_store
                .get_registered_room_identity(room_id)
                .await?;
            if let Some(identity) = identity {
                let post_slug = PostSlug::from(identity.post_slug.clone());
                self.deps
                    .driver
                    .set_room_name(
                        room_id,
                        &format!("[retired] {}/{}", raw_site_id, post_slug.as_str()),
                    )
                    .await?;
                self.deps
                    .driver
                    .remove_room_alias(&site_id, Some(&post_slug))
                    .await?;
            } else {
                warn!(
                    room_id,
                    "decommission: room has no registry identity; retiring membership only"
                );
            }
            // The AS sender leaving is not enough: guest virtual users match
            // the appservice namespace, so they must leave too or the
            // homeserver keeps pushing the room's events to us.
            self.deps.driver.leave_room(room_id).await?;
            for user_id in &virtual_users {
                self.deps.driver.leave_room_as(room_id, user_id).await?;
            }
        }

        let space_id = self
            .deps
            .site_store
            .get_site(&site_id)
            .await?
            .map(|site| site.matrix_space_id)
            .unwrap_or_default();
        if !space_id.is_empty() {
            self.deps
                .driver
                .set_room_name(&space_id, &format!("[retired] {raw_site_id}"))
                .await?;
            self.deps.driver.remove_room_alias(&site_id, None).await?;
            self.deps.driver.leave_room(&space_id).await?;
        }

        // Matrix side is fully retired: now clearing local rows cannot be
        // undone by a later backfill.
        self.deps.site_auth_store.delete_site(raw_site_id).await?;

        // Best-effort cleanup that depends on rows we just deleted: media
        // copies on the homeserver and claim-DM memberships.
        for url in media_urls {
            let Some(rest) = url.strip_prefix("mxc://") else {
                continue;
            };
            let Some((server, media_id)) = rest.split_once('/') else {
                continue;
            };
            if let Err(error) = self.deps.driver.delete_media(server, media_id).await {
                warn!(url, "decommission: media deletion failed: {:#}", error);
            }
        }
        for (user_id, dm_room_id) in claim_dms {
            if self
                .deps
                .role_claim_store
                .active_claims_in_dm_room(&user_id, &dm_room_id)
                .await?
            {
                continue;
            }
            if let Err(error) = self.deps.driver.leave_room(&dm_room_id).await {
                warn!(
                    user_id,
                    dm_room_id, "decommission: failed to leave claim DM: {:#}", error
                );
            }
        }
        Ok(())
    }
}

#[async_trait]
impl ReconcilePass for DecommissionPass {
    fn config(&self) -> &PassConfig {
        &self.config
    }

    async fn run(&self) -> Result<u64> {
        self.reconcile().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cumments_core::{
        governance::{NewRoleClaim, OWNER_LEVEL},
        models::{PostSlug, SiteId},
        ports::{
            MessageStore, RegistryStore, RoleClaimStore, SiteAuthStore, SiteStore, VirtualUserStore,
        },
        site_auth::token_hash,
        site_service::SiteService,
    };
    use cumments_store::DbStore;
    use cumments_test_utils::TestDriver;
    use std::sync::Arc;
    use tokio::sync::Notify;

    fn test_db_url(name: &str) -> String {
        let path = std::path::Path::new("/tmp").join(format!(
            "cumments-decommission-test-{}-{}.db",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        std::fs::File::create(&path).expect("create db file");
        format!("sqlite://{}", path.display())
    }

    #[tokio::test]
    async fn decommission_retires_all_rooms_users_media_and_claim_dms() {
        let store = Arc::new(
            DbStore::connect(&test_db_url("decommission"))
                .await
                .expect("connect db"),
        );
        let site = "retiring-site";
        store
            .register_site(site, &token_hash("claim"), false)
            .await
            .expect("register site");
        store
            .ensure_site_exists(site, "!space:hs")
            .await
            .expect("map space");
        assert!(store.mark_site_retiring(site).await.unwrap());

        let site_id = SiteId::new(site.to_string()).expect("site id");
        let post_slug = PostSlug::new("hello".to_string()).expect("post slug");
        store
            .register_room("!active:hs", &site_id, &post_slug)
            .await
            .expect("active room");
        store
            .register_room("!quar:hs", &site_id, &post_slug)
            .await
            .expect("quarantined room");
        store
            .quarantine_room("!quar:hs", "adoption failed", 1, None)
            .await
            .expect("quarantine");
        store
            .register_room("!old:hs", &site_id, &post_slug)
            .await
            .expect("superseded room");
        store.retire_room("!old:hs").await.expect("retire room");

        let vu1 = store
            .get_or_create_virtual_user(
                "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc",
                &site_id,
                "hs",
            )
            .await
            .expect("virtual user 1");
        let vu2 = store
            .get_or_create_virtual_user(&"A".repeat(43), &site_id, "hs")
            .await
            .expect("virtual user 2");
        store
            .record_media_upload(
                "mxc://hs/abc",
                "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc",
                site,
                Some("hello"),
            )
            .await
            .expect("media upload");
        store
            .upsert_role_claim(&NewRoleClaim {
                site_id: site.to_string(),
                room_id: String::new(),
                user_id: "@u:hs".to_string(),
                level: OWNER_LEVEL,
                token_hash: "hash".to_string(),
                expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            })
            .await
            .expect("role claim");
        store
            .set_claim_dm_room_for_user("@u:hs", "!dm:hs")
            .await
            .expect("claim dm");

        let driver = Arc::new(TestDriver::new());
        let deps = Arc::new(ReconcilerDeps {
            submission_store: store.clone(),
            registry_store: store.clone(),
            site_store: store.clone(),
            role_claim_store: store.clone(),
            governance_store: store.clone(),
            message_store: store.clone(),
            virtual_user_store: store.clone(),
            site_auth_store: store.clone(),
            driver: driver.clone(),
            site_service: Arc::new(SiteService::new(
                store.clone() as Arc<dyn cumments_core::ports::SiteStore>
            )),
        });
        let pass = DecommissionPass::new(
            deps,
            PassConfig {
                name: "decommission-test",
                interval: std::time::Duration::from_secs(60),
                wakeup: Arc::new(Notify::new()),
            },
        );
        assert_eq!(pass.run().await.expect("decommission pass"), 1);

        let mut left = driver.left.lock().await.clone();
        left.sort();
        assert_eq!(
            left,
            vec![
                "!active:hs".to_string(),
                "!dm:hs".to_string(),
                "!old:hs".to_string(),
                "!quar:hs".to_string(),
                "!space:hs".to_string(),
            ]
        );

        let mut left_as = driver.left_as.lock().await.clone();
        left_as.sort();
        assert_eq!(
            left_as,
            vec![
                ("!active:hs".to_string(), vu1.clone()),
                ("!active:hs".to_string(), vu2.clone()),
                ("!old:hs".to_string(), vu1.clone()),
                ("!old:hs".to_string(), vu2.clone()),
                ("!quar:hs".to_string(), vu1.clone()),
                ("!quar:hs".to_string(), vu2.clone()),
            ]
        );

        assert_eq!(
            *driver.deleted.lock().await,
            vec![("hs".to_string(), "abc".to_string())]
        );
        assert!(store.get_site_auth(site).await.unwrap().is_none());
        assert!(
            store
                .pending_claims_for_user("@u:hs")
                .await
                .unwrap()
                .is_empty()
        );
    }
}
