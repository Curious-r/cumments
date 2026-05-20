use anyhow::Result;
use cumments_core::{
    intents::{DeleteCommentIntent, PostCommentIntent},
    ports::{IntentStore, MatrixDriver},
    site_service::SiteService,
};
use sqlx::SqlitePool;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

/// The Reconciler acts as the Orchestrator of the background process.
/// It coordinates between the SiteService (Brain) and the MatrixDriver (Hands).
pub struct Reconciler {
    pool: SqlitePool,
    intent_store: Arc<dyn IntentStore>,
    driver: Arc<dyn MatrixDriver>,
    site_service: Arc<SiteService>,
}

impl Reconciler {
    pub fn new(
        pool: SqlitePool,
        intent_store: Arc<dyn IntentStore>,
        driver: Arc<dyn MatrixDriver>,
        site_service: Arc<SiteService>,
    ) -> Self {
        Self {
            pool,
            intent_store,
            driver,
            site_service,
        }
    }

    /// Runs the main reconciliation loop.
    pub async fn run(&self) {
        info!("Starting reconciler loop...");
        let mut interval = tokio::time::interval(Duration::from_secs(10));

        loop {
            interval.tick().await;
            tracing::debug!("Reconciler tick: Checking for new intents...");

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
        Ok(post_count + delete_count)
    }

    async fn reconcile_posts(&self) -> Result<u64> {
        let rows = sqlx::query!(
            r#"
            SELECT id, payload
            FROM intent_queue_post_comment
            WHERE status = 'pending'
            ORDER BY created_at ASC
            "#
        )
        .fetch_all(&self.pool)
        .await?;

        if rows.is_empty() {
            return Ok(0);
        }
        let num_rows = rows.len() as u64;

        for row in rows {
            let process_result: Result<()> = (async {
                let intent: PostCommentIntent = serde_json::from_str(&row.payload)?;

                // ORCHESTRATION START
                // 1. Brain: Ensure the site space is ready
                let space_id = self
                    .site_service
                    .ensure_space(&intent.site_id, self.driver.as_ref())
                    .await?;

                // 2. Hands: Ensure the post-specific room exists and is linked
                let room_id = self
                    .driver
                    .ensure_comment_room(&intent.site_id, &intent.post_slug, &space_id)
                    .await?;

                // 3. Hands: Post the actual message
                let event_id = self
                    .driver
                    .post_message(
                        &room_id,
                        &intent.content,
                        &intent.nickname,
                        &intent.author_fingerprint,
                    )
                    .await?;

                // 4. Closed-loop: Mark as waiting for sync instead of completed
                self.intent_store
                    .mark_post_intent_waiting_for_sync(row.id, &event_id)
                    .await?;
                // ORCHESTRATION END

                Ok(())
            })
            .await;

            if let Err(e) = process_result {
                warn!(
                    "Failed to reconcile post intent [{}]: {:?}. Setting status to 'failed'.",
                    row.id, e
                );
                sqlx::query!(
                    "UPDATE intent_queue_post_comment SET status = 'failed', updated_at = strftime('%s','now') WHERE id = ?",
                    row.id
                )
                .execute(&self.pool)
                .await?;
            }
        }
        Ok(num_rows)
    }

    async fn reconcile_deletions(&self) -> Result<u64> {
        let rows = sqlx::query!(
            r#"
            SELECT id, payload
            FROM intent_queue_delete_comment
            WHERE status = 'pending'
            ORDER BY created_at ASC
            "#
        )
        .fetch_all(&self.pool)
        .await?;

        if rows.is_empty() {
            return Ok(0);
        }
        let num_rows = rows.len() as u64;

        for row in rows {
            let process_result: Result<()> = (async {
                let intent: DeleteCommentIntent = serde_json::from_str(&row.payload)?;

                // Hands: Perform the redaction
                self.driver.redact_message(&intent.site_id, &intent.post_slug, &intent.event_id).await?;

                // Concepts: Move to waiting_for_sync
                sqlx::query!(
                    "UPDATE intent_queue_delete_comment SET status = 'waiting_for_sync', updated_at = strftime('%s','now') WHERE id = ?",
                    row.id
                )
                .execute(&self.pool)
                .await?;

                Ok(())
            }).await;

            if let Err(e) = process_result {
                warn!(
                    "Failed to reconcile delete intent [{}]: {:?}. Setting status to 'failed'.",
                    row.id, e
                );
                sqlx::query!(
                    "UPDATE intent_queue_delete_comment SET status = 'failed', updated_at = strftime('%s','now') WHERE id = ?",
                    row.id
                )
                .execute(&self.pool)
                .await?;
            }
        }
        Ok(num_rows)
    }
}
