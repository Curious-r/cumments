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
        let update_count = self.reconcile_updates().await?;
        Ok(post_count + delete_count + update_count)
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
                    "UPDATE intent_queue_post_comment SET status = 'failed', updated_at = CURRENT_TIMESTAMP WHERE id = ?",
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
                    "UPDATE intent_queue_delete_comment SET status = 'waiting_for_sync', updated_at = CURRENT_TIMESTAMP WHERE id = ?",
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
                    "UPDATE intent_queue_delete_comment SET status = 'failed', updated_at = CURRENT_TIMESTAMP WHERE id = ?",
                    row.id
                )
                .execute(&self.pool)
                .await?;
            }
        }
        Ok(num_rows)
    }

    async fn reconcile_updates(&self) -> Result<u64> {
        let rows = sqlx::query!(
            r#"
            SELECT id, site_id, post_slug, event_id, content, author_fingerprint
            FROM intent_queue_update_comment
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
                let site_id = row.site_id.clone().into();
                let post_slug = row.post_slug.clone().into();

                // 1. Brain: Ensure site space
                let space_id = self
                    .site_service
                    .ensure_space(&site_id, self.driver.as_ref())
                    .await?;

                // 2. Hands: Ensure room
                let room_id = self
                    .driver
                    .ensure_comment_room(&site_id, &post_slug, &space_id)
                    .await?;

                // 3. Hands: Fetch original nickname to maintain it
                let nickname = sqlx::query_scalar!(
                    "SELECT author_nickname FROM comments WHERE event_id = ?",
                    row.event_id
                )
                .fetch_optional(&self.pool)
                .await?
                .flatten()
                .unwrap_or_else(|| "Guest".to_string());

                // 4. Hands: Perform the update (m.replace)
                self.driver
                    .update_message(
                        &room_id,
                        &row.event_id,
                        &row.content,
                        &nickname,
                        &row.author_fingerprint,
                    )
                    .await?;

                // 5. Closed-loop: Mark as waiting for sync
                self.intent_store
                    .mark_update_intent_waiting_for_sync(row.id.unwrap())
                    .await?;

                Ok(())
            })
            .await;

            if let Err(e) = process_result {
                warn!(
                    "Failed to reconcile update intent [{:?}]: {:?}. Setting status to 'failed'.",
                    row.id, e
                );
                sqlx::query!(
                    "UPDATE intent_queue_update_comment SET status = 'failed', updated_at = CURRENT_TIMESTAMP WHERE id = ?",
                    row.id
                )
                .execute(&self.pool)
                .await?;
            }
        }
        Ok(num_rows)
    }
}
