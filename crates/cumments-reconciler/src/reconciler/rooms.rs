//! Room lifecycle cleanup: retires AS-managed memberships from superseded
//! rooms so the homeserver stops pushing their events to the appservice.

use super::*;
use anyhow::Result;
use async_trait::async_trait;
use cumments_core::models::SiteId;
use tracing::{info, warn};

/// Leaves superseded rooms as the AS sender and every site virtual user.
pub struct RoomCleanupPass {
    deps: Arc<ReconcilerDeps>,
    config: PassConfig,
}

impl RoomCleanupPass {
    pub fn new(deps: Arc<ReconcilerDeps>, config: PassConfig) -> Self {
        Self { deps, config }
    }

    async fn reconcile(&self) -> Result<u64> {
        let mut cleaned = 0u64;
        for room_id in self.deps.registry_store.list_superseded_rooms().await? {
            let Some(identity) = self
                .deps
                .registry_store
                .get_registered_room_identity(&room_id)
                .await?
            else {
                continue;
            };
            let Ok(site_id) = SiteId::new(identity.site_id) else {
                continue;
            };
            let virtual_users = self
                .deps
                .virtual_user_store
                .list_virtual_users_for_site(&site_id)
                .await?;

            if let Err(error) = self.deps.driver.leave_room(&room_id).await {
                warn!(room_id, "room cleanup: sender leave failed: {:#}", error);
                continue;
            }
            for user_id in &virtual_users {
                if let Err(error) = self.deps.driver.leave_room_as(&room_id, user_id).await {
                    warn!(
                        room_id,
                        user_id, "room cleanup: virtual user leave failed: {:#}", error
                    );
                }
            }
            cleaned += 1;
            info!(
                room_id,
                "retired AS-managed memberships from superseded room"
            );
        }
        Ok(cleaned)
    }
}

#[async_trait]
impl ReconcilePass for RoomCleanupPass {
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
        models::{PageSlug, SiteId},
        ports::{RegistryStore, VirtualUserStore},
    };
    use cumments_store::DbStore;
    use cumments_test_utils::TestDriver;
    use std::sync::Arc;
    use tokio::sync::Notify;

    fn test_db_url(name: &str) -> String {
        let path = std::path::Path::new("/tmp").join(format!(
            "cumments-room-cleanup-test-{}-{}.db",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        std::fs::File::create(&path).expect("create db file");
        format!("sqlite://{}", path.display())
    }

    #[tokio::test]
    async fn room_cleanup_leaves_superseded_rooms_as_all_as_users() {
        let store = Arc::new(
            DbStore::connect(&test_db_url("superseded"))
                .await
                .expect("connect db"),
        );
        let site_id = SiteId::new("my-blog".to_string()).expect("site id");
        let page_slug = PageSlug::new("hello".to_string()).expect("page slug");
        store
            .register_room("!old:hs", &site_id, &page_slug)
            .await
            .expect("register room");
        store.retire_room("!old:hs").await.expect("supersede room");

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
            site_service: Arc::new(cumments_core::site_service::SiteService::new(
                store.clone() as Arc<dyn cumments_core::ports::SiteStore>
            )),
        });
        let pass = RoomCleanupPass::new(
            deps,
            PassConfig {
                name: "rooms-test",
                interval: std::time::Duration::from_secs(60),
                wakeup: Arc::new(Notify::new()),
            },
        );
        assert_eq!(pass.run().await.expect("room cleanup pass"), 1);

        assert_eq!(*driver.left.lock().await, vec!["!old:hs".to_string()]);
        let mut left_as = driver.left_as.lock().await.clone();
        left_as.sort();
        assert_eq!(
            left_as,
            vec![("!old:hs".to_string(), vu1), ("!old:hs".to_string(), vu2),]
        );
    }
}
