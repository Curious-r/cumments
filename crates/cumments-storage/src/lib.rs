use anyhow::Result;
use async_trait::async_trait;
use cumments_core::intents::PostCommentIntent;
use cumments_core::ports::IntentRepository;
use sqlx::SqlitePool;

/// A wrapper around the database pool that provides concrete implementations
/// of the storage-related ports defined in `cumments-core`.
pub struct Storage {
    pool: SqlitePool,
}

impl Storage {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl IntentRepository for Storage {
    /// Saves a `PostCommentIntent` to the `intent_queue_post_comment` table.
    /// The intent is serialized to JSON for storage. The Reconciler will
    /// later deserialize it for processing.
    async fn save_post_comment_intent(&self, intent: &PostCommentIntent) -> Result<()> {
        let intent_json = serde_json::to_string(intent)?;

        // This query assumes a table named `intent_queue_post_comment` exists.
        // We will create a migration for it in a later step.
        sqlx::query!(
            r#"
            INSERT INTO intent_queue_post_comment (payload)
            VALUES (?)
            "#,
            intent_json
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}
