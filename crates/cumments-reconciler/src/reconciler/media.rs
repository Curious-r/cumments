//! Orphan media cleanup: forget uploads that were never referenced by a
//! comment.
//!
//! The homeserver remains the authority on physical media lifecycle;
//! Cumments manages only its own references and local upload records.

use super::*;
use anyhow::Result;
use async_trait::async_trait;
use tracing::{info, warn};

/// An upload counts as orphaned once it has been unreferenced this long.
const ORPHAN_AGE: chrono::Duration = chrono::Duration::hours(24);

/// Periodic sweep for unreferenced visitor uploads.
pub struct MediaCleanupPass {
    deps: Arc<ReconcilerDeps>,
    config: PassConfig,
}

impl MediaCleanupPass {
    pub fn new(deps: Arc<ReconcilerDeps>, config: PassConfig) -> Self {
        Self { deps, config }
    }

    async fn reconcile(&self) -> Result<u64> {
        let cutoff = chrono::Utc::now() - ORPHAN_AGE;
        let orphans = self
            .deps
            .message_store
            .list_unused_media_before(cutoff)
            .await?;
        let mut cleaned = 0u64;
        for url in orphans {
            match self.deps.message_store.delete_media_upload(&url).await {
                Ok(()) => {
                    cleaned += 1;
                    info!(url, "orphan media record removed");
                }
                Err(error) => {
                    warn!(url, "failed to forget orphan media: {error:#}");
                }
            }
        }
        Ok(cleaned)
    }
}

#[async_trait]
impl ReconcilePass for MediaCleanupPass {
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
    use cumments_core::ports::MessageStore;
    use cumments_store::{DbStore, sea_orm};
    use cumments_test_utils::TestDriver;
    use sea_orm::ConnectionTrait;
    use tokio::sync::Notify;

    fn test_db_url(name: &str) -> String {
        let path = std::path::Path::new("/tmp").join(format!(
            "cumments-media-cleanup-test-{}-{}.db",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        std::fs::File::create(&path).expect("create db file");
        format!("sqlite://{}", path.display())
    }

    #[tokio::test]
    async fn media_cleanup_removes_unreferenced_records_without_matrix_deletion() {
        let store = Arc::new(
            DbStore::connect(&test_db_url("cleanup"))
                .await
                .expect("connect db"),
        );
        let driver = Arc::new(TestDriver::new());
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
            site_service: Arc::new(cumments_core::site_service::SiteService::new(
                store.clone() as Arc<dyn cumments_core::ports::SiteStore>
            )),
            profile_store: None,
            media_resolver: None,
        });
        let pass = MediaCleanupPass::new(
            deps,
            PassConfig {
                name: "media-cleanup-test",
                interval: std::time::Duration::from_secs(60),
                wakeup: Arc::new(Notify::new()),
            },
        );

        // Record an unreferenced upload created in the past (beyond ORPHAN_AGE).
        store
            .record_media_upload(
                "mxc://hs/orphan123",
                "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc",
                "test-site",
                Some("hello"),
            )
            .await
            .expect("record media upload");

        // Manually backdate the created_at in the database so it qualifies as an orphan.
        let old_time = chrono::Utc::now() - chrono::Duration::hours(25);
        store
            .connection()
            .execute_unprepared(&format!(
                "UPDATE media_uploads SET created_at = '{}' WHERE mxc_url = 'mxc://hs/orphan123'",
                old_time.to_rfc3339()
            ))
            .await
            .expect("backdate media upload");

        let cleaned = pass.run().await.expect("media cleanup pass run");
        assert_eq!(cleaned, 1);

        // Verify the local record was deleted.
        let remaining = store
            .list_unused_media_before(chrono::Utc::now())
            .await
            .expect("list unused");
        assert!(remaining.is_empty());
    }
}
