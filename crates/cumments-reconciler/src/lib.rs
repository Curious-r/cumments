use anyhow::Result;
use cumments_core::{
    intents::{DeleteCommentIntent, PostCommentIntent},
    ports::MatrixOperator,
    site_service::SiteService,
};
use sqlx::SqlitePool;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

/// The Reconciler acts as the Orchestrator of the background process.
/// It coordinates between the SiteService (Brain) and the MatrixOperator (Hands).
pub struct Reconciler {
    pool: SqlitePool,
    operator: Arc<dyn MatrixOperator>,
    site_service: Arc<SiteService>,
}

impl Reconciler {
    pub fn new(
        pool: SqlitePool,
        operator: Arc<dyn MatrixOperator>,
        site_service: Arc<SiteService>,
    ) -> Self {
        Self {
            pool,
            operator,
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

            match self.process_intents().await {
                Ok(count) => {
                    if count > 0 {
                        info!("Successfully processed {} intents.", count);
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to process intents: {:?}", e);
                }
            }
        }
    }

    /// Fetches and processes all types of pending intents from the database.
    async fn process_intents(&self) -> Result<u64> {
        let post_count = self.process_post_intents().await?;
        let delete_count = self.process_delete_intents().await?;
        Ok(post_count + delete_count)
    }

    async fn process_post_intents(&self) -> Result<u64> {
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
                let space_id = self.site_service.ensure_space(&intent.site_id, self.operator.as_ref()).await?;

                // 2. Hands: Ensure the post-specific room exists and is linked
                let room_id = self.operator.ensure_comment_room(&intent.site_id, &intent.post_slug, &space_id).await?;

                // 3. Hands: Post the actual message
                self.operator.post_message(&room_id, &intent.content, &intent.nickname).await?;
                // ORCHESTRATION END

                sqlx::query!(
                    "UPDATE intent_queue_post_comment SET status = 'completed', updated_at = strftime('%s','now') WHERE id = ?",
                    row.id
                )
                .execute(&self.pool)
                .await?;
                Ok(())
            }).await;

            if let Err(e) = process_result {
                warn!(
                    "Failed to process post intent [{}]: {:?}. Setting status to 'failed'.",
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

    async fn process_delete_intents(&self) -> Result<u64> {
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
                self.operator.redact_message(&intent.site_id, &intent.post_slug, &intent.event_id).await?;

                sqlx::query!(
                    "UPDATE intent_queue_delete_comment SET status = 'completed', updated_at = strftime('%s','now') WHERE id = ?",
                    row.id
                )
                .execute(&self.pool)
                .await?;
                Ok(())
            }).await;

            if let Err(e) = process_result {
                warn!(
                    "Failed to process delete intent [{}]: {:?}. Setting status to 'failed'.",
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
