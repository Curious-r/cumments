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
        models::{CommentMedia, PostSlug, RoomEventPage, SiteId},
        ports::{MatrixDriver, RegistryStore, VirtualUserStore},
    };
    use cumments_store::DbStore;
    use std::sync::Arc;
    use tokio::sync::{Mutex, Notify};

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

    struct TestDriver {
        left: Mutex<Vec<String>>,
        left_as: Mutex<Vec<(String, String)>>,
    }

    impl TestDriver {
        fn new() -> Self {
            Self {
                left: Mutex::new(Vec::new()),
                left_as: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl MatrixDriver for TestDriver {
        async fn ensure_comment_room(
            &self,
            _site_id: &SiteId,
            _post_slug: &PostSlug,
            _space_id: &str,
            _candidate_room_id: Option<&str>,
        ) -> anyhow::Result<String> {
            unimplemented!("not used in this test")
        }
        async fn create_site_space(&self, _site_id: &SiteId) -> anyhow::Result<String> {
            unimplemented!("not used in this test")
        }
        async fn set_room_name(&self, _room_id: &str, _name: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn leave_room(&self, room_id: &str) -> anyhow::Result<()> {
            self.left.lock().await.push(room_id.to_string());
            Ok(())
        }
        async fn leave_room_as(&self, room_id: &str, user_id: &str) -> anyhow::Result<()> {
            self.left_as
                .lock()
                .await
                .push((room_id.to_string(), user_id.to_string()));
            Ok(())
        }
        async fn join_room(&self, _room_id: &str) -> anyhow::Result<()> {
            unimplemented!("not used in this test")
        }
        async fn remove_room_alias(
            &self,
            _site_id: &SiteId,
            _post_slug: Option<&PostSlug>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete_media(&self, _server: &str, _media_id: &str) -> anyhow::Result<bool> {
            unimplemented!("not used in this test")
        }
        async fn upload_media(
            &self,
            _bytes: bytes::Bytes,
            _filename: &str,
            _mimetype: &str,
            _author_public_key: &str,
            _site_id: &SiteId,
        ) -> anyhow::Result<String> {
            unimplemented!("not used in this test")
        }
        #[allow(clippy::too_many_arguments)]
        async fn post_message(
            &self,
            _room_id: &str,
            _content: &str,
            _media: Option<&CommentMedia>,
            _display_name: &str,
            _author_public_key: &str,
            _author_signature: &str,
            _author_challenge: &str,
            _site_id: &SiteId,
            _reply_to: Option<&str>,
            _reply_to_body: Option<&str>,
            _reply_to_sender: Option<&str>,
            _submission_id: Option<i64>,
            _txn_id: &str,
        ) -> anyhow::Result<String> {
            unimplemented!("not used in this test")
        }
        async fn react_message(
            &self,
            _room_id: &str,
            _target_event_id: &str,
            _key: &str,
            _site_id: &SiteId,
            _author_public_key: &str,
            _author_signature: &str,
            _author_challenge: &str,
        ) -> anyhow::Result<()> {
            unimplemented!("not used in this test")
        }
        async fn vote_poll(
            &self,
            _room_id: &str,
            _poll_event_id: &str,
            _answer_id: &str,
            _site_id: &SiteId,
            _author_public_key: &str,
            _author_signature: &str,
            _author_challenge: &str,
        ) -> anyhow::Result<()> {
            unimplemented!("not used in this test")
        }
        #[allow(clippy::too_many_arguments)]
        async fn post_location(
            &self,
            _room_id: &str,
            _geo_uri: &str,
            _description: Option<&str>,
            _display_name: &str,
            _site_id: &SiteId,
            _author_public_key: &str,
            _author_signature: &str,
            _author_challenge: &str,
            _submission_id: Option<i64>,
            _txn_id: &str,
        ) -> anyhow::Result<String> {
            unimplemented!("not used in this test")
        }
        #[allow(clippy::too_many_arguments)]
        async fn update_message(
            &self,
            _room_id: &str,
            _event_id: &str,
            _new_content: &str,
            _display_name: &str,
            _author_public_key: &str,
            _author_signature: &str,
            _author_challenge: &str,
            _site_id: &SiteId,
            _submission_id: Option<i64>,
            _txn_id: &str,
        ) -> anyhow::Result<String> {
            unimplemented!("not used in this test")
        }
        #[allow(clippy::too_many_arguments)]
        async fn redact_message(
            &self,
            _room_id: &str,
            _event_id: &str,
            _submission_id: Option<i64>,
            _proof: Option<&serde_json::Value>,
            _txn_id: &str,
        ) -> anyhow::Result<String> {
            unimplemented!("not used in this test")
        }
        async fn get_room_events(
            &self,
            _room_id: &str,
            _from: Option<&str>,
            _limit: u32,
        ) -> anyhow::Result<RoomEventPage> {
            unimplemented!("not used in this test")
        }
        async fn get_joined_rooms(&self) -> anyhow::Result<Vec<String>> {
            unimplemented!("not used in this test")
        }
        async fn get_joined_members(&self, _room_id: &str) -> anyhow::Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn get_room_metadata(
            &self,
            _room_id: &str,
        ) -> anyhow::Result<Option<serde_json::Value>> {
            unimplemented!("not used in this test")
        }
        async fn get_room_canonical_alias(&self, _room_id: &str) -> anyhow::Result<Option<String>> {
            unimplemented!("not used in this test")
        }
        async fn event_exists(&self, _room_id: &str, _event_id: &str) -> anyhow::Result<bool> {
            unimplemented!("not used in this test")
        }
        fn sender_user_id(&self) -> Option<String> {
            Some("@_cumments_bot:hs".to_string())
        }
        async fn get_room_power_levels(
            &self,
            _room_id: &str,
        ) -> anyhow::Result<Option<serde_json::Value>> {
            unimplemented!("not used in this test")
        }
        async fn set_room_power_levels(
            &self,
            _room_id: &str,
            _content: &serde_json::Value,
        ) -> anyhow::Result<()> {
            unimplemented!("not used in this test")
        }
        async fn invite_user(&self, _room_id: &str, _user_id: &str) -> anyhow::Result<()> {
            unimplemented!("not used in this test")
        }
    }

    #[tokio::test]
    async fn room_cleanup_leaves_superseded_rooms_as_all_as_users() {
        let store = Arc::new(
            DbStore::connect(&test_db_url("superseded"))
                .await
                .expect("connect db"),
        );
        let site_id = SiteId::new("my-blog".to_string()).expect("site id");
        let post_slug = PostSlug::new("hello".to_string()).expect("post slug");
        store
            .register_room("!old:hs", &site_id, &post_slug)
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
