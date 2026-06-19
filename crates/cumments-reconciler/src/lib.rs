use anyhow::Result;
use cumments_core::{
    ports::{CommentStore, IntentStore, MatrixDriver, RegistryStore},
    site_service::SiteService,
};
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

use tokio::sync::Notify;

/// The Reconciler acts as the Orchestrator of the background process.
/// It coordinates between the SiteService (Brain) and the MatrixDriver (Hands).
pub struct Reconciler {
    intent_store: Arc<dyn IntentStore>,
    registry_store: Arc<dyn RegistryStore>,
    comment_store: Arc<dyn CommentStore>,
    driver: Arc<dyn MatrixDriver>,
    site_service: Arc<SiteService>,
    notify: Arc<Notify>,
}

impl Reconciler {
    pub fn new(
        intent_store: Arc<dyn IntentStore>,
        registry_store: Arc<dyn RegistryStore>,
        comment_store: Arc<dyn CommentStore>,
        driver: Arc<dyn MatrixDriver>,
        site_service: Arc<SiteService>,
        notify: Arc<Notify>,
    ) -> Self {
        Self {
            intent_store,
            registry_store,
            comment_store,
            driver,
            site_service,
            notify,
        }
    }

    /// Runs the main reconciliation loop.
    pub async fn run(&self) {
        info!("Starting reactive reconciler loop...");
        // Fallback interval for retries and periodic cleanup
        let mut interval = tokio::time::interval(Duration::from_secs(60));

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    tracing::debug!("Reconciler: Periodic scan triggered.");
                }
                _ = self.notify.notified() => {
                    tracing::debug!("Reconciler: Instant wake-up triggered by notification.");
                }
            }

            match self.reconcile().await {
                Ok(count) => {
                    if count > 0 {
                        info!("Successfully reconciled {} intents.", count);
                    }
                }
                Err(e) => {
                    tracing::error!("Reconciliation failed: {:?}", e);
                }
            }
        }
    }

    /// Reconciles all types of pending intents from the database.
    async fn reconcile(&self) -> Result<u64> {
        let post_count = self.reconcile_posts().await?;
        let delete_count = self.reconcile_deletions().await?;
        let update_count = self.reconcile_updates().await?;
        Ok(post_count + delete_count + update_count)
    }

    async fn reconcile_posts(&self) -> Result<u64> {
        let intents = self.intent_store.get_pending_post_intents().await?;

        if intents.is_empty() {
            return Ok(0);
        }
        let num_intents = intents.len() as u64;

        for (id, intent) in intents {
            let process_result: Result<()> = (async {
                // ORCHESTRATION START
                // 1. Brain: Ensure the site space is ready
                let space_id = self
                    .site_service
                    .ensure_space(&intent.site_id, self.driver.as_ref())
                    .await?;

                // 2. Registry: Check for existing room in local cache (O(1) hint)
                let candidate_room_id = self
                    .registry_store
                    .get_registered_room(&intent.site_id, &intent.post_slug)
                    .await?;

                // 3. Hands: Ensure the post-specific room exists and is linked
                let room_id = self
                    .driver
                    .ensure_comment_room(
                        &intent.site_id,
                        &intent.post_slug,
                        &space_id,
                        candidate_room_id.as_deref(),
                    )
                    .await?;

                // 3b. Registry: Write back the room mapping immediately
                // This is critical for AS mode where no sync loop backs us up.
                self.registry_store
                    .register_room(&room_id, &intent.site_id, &intent.post_slug)
                    .await?;

                // 4. Hands: Post the actual message
                let event_id = self
                    .driver
                    .post_message(
                        &room_id,
                        &intent.content,
                        &intent.nickname,
                        &intent.author_fingerprint,
                        &intent.site_id,
                    )
                    .await?;

                // 5. Closed-loop: Mark as waiting for sync instead of completed
                self.intent_store
                    .mark_post_intent_waiting_for_sync(id, &event_id)
                    .await?;
                // ORCHESTRATION END

                Ok(())
            })
            .await;

            if let Err(e) = process_result {
                warn!(
                    "Failed to reconcile post intent [{}]: {:?}. Setting status to 'failed'.",
                    id, e
                );
                self.intent_store.mark_post_intent_failed(id).await?;
            }
        }
        Ok(num_intents)
    }

    async fn reconcile_deletions(&self) -> Result<u64> {
        let intents = self.intent_store.get_pending_delete_intents().await?;

        if intents.is_empty() {
            return Ok(0);
        }
        let num_intents = intents.len() as u64;

        for (id, intent) in intents {
            let process_result: Result<()> = (async {
                // 1. Registry: Locate the room ID for this deletion
                let room_id = self
                    .registry_store
                    .get_registered_room(&intent.site_id, &intent.post_slug)
                    .await?;

                let room_id = room_id.ok_or_else(|| {
                    anyhow::anyhow!(
                        "Room not found in registry for site {} post {}",
                        intent.site_id.as_str(),
                        intent.post_slug.as_str()
                    )
                })?;

                // 2. Hands: Perform the redaction
                self.driver
                    .redact_message(&room_id, &intent.event_id)
                    .await?;

                // 3. Concepts: Move to waiting_for_sync
                self.intent_store
                    .mark_delete_intent_waiting_for_sync(id)
                    .await?;

                Ok(())
            })
            .await;

            if let Err(e) = process_result {
                warn!(
                    "Failed to reconcile delete intent [{}]: {:?}. Setting status to 'failed'.",
                    id, e
                );
                self.intent_store.mark_delete_intent_failed(id).await?;
            }
        }
        Ok(num_intents)
    }

    async fn reconcile_updates(&self) -> Result<u64> {
        let intents = self.intent_store.get_pending_update_intents().await?;

        if intents.is_empty() {
            return Ok(0);
        }
        let num_intents = intents.len() as u64;

        for (id, intent) in intents {
            let process_result: Result<()> = (async {
                // 1. Brain: Ensure site space
                let space_id = self
                    .site_service
                    .ensure_space(&intent.site_id, self.driver.as_ref())
                    .await?;

                // 2. Registry: Check for existing room in local cache (O(1) hint)
                let candidate_room_id = self
                    .registry_store
                    .get_registered_room(&intent.site_id, &intent.post_slug)
                    .await?;

                // 3. Hands: Ensure room
                let room_id = self
                    .driver
                    .ensure_comment_room(
                        &intent.site_id,
                        &intent.post_slug,
                        &space_id,
                        candidate_room_id.as_deref(),
                    )
                    .await?;

                // 3b. Registry: Write back the room mapping immediately
                self.registry_store
                    .register_room(&room_id, &intent.site_id, &intent.post_slug)
                    .await?;

                // 4. Hands: Fetch original nickname to maintain it
                let nickname = self
                    .comment_store
                    .get_author_nickname(&intent.event_id)
                    .await?
                    .unwrap_or_else(|| "Guest".to_string());

                // 5. Hands: Perform the update (m.replace)
                self.driver
                    .update_message(
                        &room_id,
                        &intent.event_id,
                        &intent.content,
                        &nickname,
                        &intent.author_fingerprint,
                        &intent.site_id,
                    )
                    .await?;

                // 6. Closed-loop: Mark as waiting for sync
                self.intent_store
                    .mark_update_intent_waiting_for_sync(id)
                    .await?;

                Ok(())
            })
            .await;

            if let Err(e) = process_result {
                warn!(
                    "Failed to reconcile update intent [{}]: {:?}. Setting status to 'failed'.",
                    id, e
                );
                self.intent_store.mark_update_intent_failed(id).await?;
            }
        }
        Ok(num_intents)
    }
}
