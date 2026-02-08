use anyhow::Result;
use cumments_core::intents::PostCommentIntent;
use cumments_operator::MatrixOperator;
use sqlx::SqlitePool;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

pub struct Reconciler {
    pool: SqlitePool,
    operator: Arc<dyn MatrixOperator>,
}

impl Reconciler {
    pub fn new(pool: SqlitePool, operator: Arc<dyn MatrixOperator>) -> Self {
        Self { pool, operator }
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

    /// Fetches and processes pending intents from the database.
    async fn process_intents(&self) -> Result<u64> {
        // 1. Fetch pending intents
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

        // 2. Process each intent
        for row in rows {
            // Use a block to handle errors for a single intent without stopping the whole loop
            let process_result: Result<()> = (async {
                let intent: PostCommentIntent = serde_json::from_str(&row.payload)?;

                // Use the operator to do the work
                self.operator.post_comment(&intent).await?;

                // 3. Update the intent's status to 'completed'
                sqlx::query!(
                    "UPDATE intent_queue_post_comment SET status = 'completed', updated_at = strftime('%s','now') WHERE id = ?",
                    row.id
                )
                .execute(&self.pool)
                .await?;

                Ok(())
            }).await;

            if let Err(e) = process_result {
                tracing::error!(
                    "Failed to process intent [{}]: {:?}. Setting status to 'failed'.",
                    row.id,
                    e
                );
                // Mark as failed so we don't retry it indefinitely
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
}
