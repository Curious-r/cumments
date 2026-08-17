//! Page-level room retirement: leaves a retired comment room's Matrix side
//! and then clears its local projections.
//!
//! Order matters, same as site retirement: Matrix first (rename, alias
//! removal, AS sender and virtual users leave), local cleanup second. Once
//! the AS sender has left, `backfill` cannot discover the room again, so
//! deleting its local rows cannot resurrect it.

use super::*;
use anyhow::Result;
use async_trait::async_trait;
use tracing::warn;

/// Retires every room marked `Retired` (post-level retirement).
pub struct RoomRetirementPass {
    deps: Arc<ReconcilerDeps>,
    config: PassConfig,
}

impl RoomRetirementPass {
    pub fn new(deps: Arc<ReconcilerDeps>, config: PassConfig) -> Self {
        Self { deps, config }
    }

    async fn reconcile(&self) -> Result<u64> {
        let retired = self.deps.registry_store.list_retired_rooms().await?;
        let mut finished = 0u64;
        for room_id in retired {
            if let Err(error) = self.retire_room(&room_id).await {
                warn!(room_id, "room retirement failed: {:#}", error);
                continue;
            }
            finished += 1;
        }
        Ok(finished)
    }

    async fn retire_room(&self, room_id: &str) -> Result<()> {
        let Some(identity) = self
            .deps
            .registry_store
            .get_registered_room_identity(room_id)
            .await?
        else {
            // Already cleaned up by an earlier pass.
            return Ok(());
        };
        let site_id = SiteId::new(identity.site_id.clone())
            .map_err(|_| anyhow::anyhow!("invalid site id in registry: {}", identity.site_id))?;
        let page_slug = PageSlug::new(identity.page_slug.clone()).map_err(|_| {
            anyhow::anyhow!("invalid page slug in registry: {}", identity.page_slug)
        })?;
        let virtual_users = self
            .deps
            .virtual_user_store
            .list_virtual_users_for_site(&site_id)
            .await?;

        self.deps
            .driver
            .set_room_name(
                room_id,
                &format!("[retired] {}/{}", site_id.as_str(), page_slug.as_str()),
            )
            .await?;
        self.deps
            .driver
            .remove_room_alias(&site_id, Some(&page_slug))
            .await?;
        self.deps.driver.leave_room(room_id).await?;
        for user_id in &virtual_users {
            self.deps.driver.leave_room_as(room_id, user_id).await?;
        }

        // Matrix side is gone: clearing local rows cannot be undone by a
        // later backfill.
        self.deps.site_auth_store.delete_room_local(room_id).await?;
        Ok(())
    }
}

#[async_trait]
impl ReconcilePass for RoomRetirementPass {
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
            "cumments-room-retirement-test-{}-{}.db",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        std::fs::File::create(&path).expect("create db file");
        format!("sqlite://{}", path.display())
    }

    #[tokio::test]
    async fn retirement_leaves_matrix_then_clears_local_rows() {
        let store = Arc::new(
            DbStore::connect(&test_db_url("retire"))
                .await
                .expect("connect db"),
        );
        let site_id = SiteId::new("my-blog".to_string()).expect("site id");
        let page_slug = PageSlug::new("hello".to_string()).expect("page slug");
        store
            .register_room("!room:hs", &site_id, &page_slug)
            .await
            .expect("register room");
        assert!(
            store
                .mark_room_retired("!room:hs")
                .await
                .expect("mark retired")
        );
        let vu = store
            .get_or_create_virtual_user(
                "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc",
                &site_id,
                "hs",
            )
            .await
            .expect("virtual user");

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
        let pass = RoomRetirementPass::new(
            deps,
            PassConfig {
                name: "room-retirement-test",
                interval: std::time::Duration::from_secs(60),
                wakeup: Arc::new(Notify::new()),
            },
        );
        assert_eq!(
            pass.run().await.expect("room retirement pass"),
            1,
            "one room must be processed"
        );

        assert_eq!(*driver.left.lock().await, vec!["!room:hs".to_string()]);
        assert_eq!(
            *driver.left_as.lock().await,
            vec![("!room:hs".to_string(), vu)]
        );
        assert!(
            store
                .get_registered_room_identity("!room:hs")
                .await
                .expect("registry query")
                .is_none(),
            "local registry row must be cleared after the Matrix side left"
        );

        // A second sweep is a no-op (no retired rows remain).
        assert_eq!(pass.run().await.expect("second pass"), 0);
    }
}
