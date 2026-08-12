use super::DbStore;
use crate::entities::{
    active_enums::IntentStatus, intent_queue_delete_comment, intent_queue_post_comment,
    intent_queue_update_comment,
};
use anyhow::Result;
use async_trait::async_trait;
use cumments_core::intents::{
    DeleteCommentIntent, PendingDeleteIntent, PendingPostIntent, PendingUpdateIntent,
    PostCommentIntent, StuckPostIntent, UpdateCommentIntent,
};
use cumments_core::ports::IntentStore;
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    Set, UpdateMany,
};
use tracing::warn;

/// Maximum failed attempts before an intent is marked `failed` (dead-lettered).
const MAX_RETRIES: i64 = 5;
/// Base exponential-backoff delay after the first failure.
const BASE_BACKOFF_SECS: i64 = 30;
/// Upper bound for the backoff delay.
const MAX_BACKOFF_SECS: i64 = 1800;
/// Minimum time between two timeout confirmation passes for the same post
/// intent. Matches the reconciler's 60s fallback interval so the three
/// confirmations required before dead-lettering are genuinely spread across
/// reconcile cycles.
const TIMEOUT_CONFIRMATION_COOLDOWN_MS: i64 = 60_000;

/// Exponential backoff delay for the *next* attempt, based on the number of
/// failures already recorded.
fn backoff_after(retry_count: i64) -> chrono::Duration {
    let shift = retry_count.clamp(0, 6) as u32;
    let secs = BASE_BACKOFF_SECS
        .saturating_mul(1i64 << shift)
        .min(MAX_BACKOFF_SECS);
    chrono::Duration::seconds(secs)
}

/// Filter for rows whose backoff window has passed (never attempted yet, or
/// `next_attempt_at` in the past).
fn attempt_due<C>(column: C) -> Condition
where
    C: ColumnTrait,
{
    Condition::any()
        .add(column.is_null())
        .add(column.lte(chrono::Utc::now()))
}

#[async_trait]
impl IntentStore for DbStore {
    async fn save_post_intent(&self, intent: &PostCommentIntent) -> Result<i64> {
        let payload = serde_json::to_string(intent)?;

        let active_model = intent_queue_post_comment::ActiveModel {
            payload: Set(payload),
            status: Set(IntentStatus::Pending),
            retry_count: Set(0),
            timeout_confirmations: Set(0),
            timeout_check_errors: Set(0),
            last_timeout_confirmation_at: Set(None),
            author_public_key: Set(Some(intent.author_public_key.clone())),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        };

        let result = intent_queue_post_comment::Entity::insert(active_model)
            .exec(&self.db)
            .await?;
        Ok(result.last_insert_id)
    }

    async fn save_delete_intent(&self, intent: &DeleteCommentIntent) -> Result<i64> {
        let payload = serde_json::to_string(intent)?;

        let active_model = intent_queue_delete_comment::ActiveModel {
            payload: Set(payload),
            status: Set(IntentStatus::Pending),
            target_event_id: Set(Some(intent.event_id.clone())),
            retry_count: Set(0),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        };

        let result = intent_queue_delete_comment::Entity::insert(active_model)
            .exec(&self.db)
            .await?;
        Ok(result.last_insert_id)
    }

    async fn save_update_intent(&self, intent: &UpdateCommentIntent) -> Result<i64> {
        let active_model = intent_queue_update_comment::ActiveModel {
            site_id: Set(intent.site_id.as_str().to_owned()),
            post_slug: Set(intent.post_slug.as_str().to_owned()),
            event_id: Set(intent.event_id.clone()),
            content: Set(intent.content.clone()),
            author_public_key: Set(Some(intent.author_public_key.clone())),
            author_signature: Set(Some(intent.author_signature.clone())),
            author_challenge: Set(Some(intent.author_challenge.clone())),
            status: Set(IntentStatus::Pending),
            retry_count: Set(0),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        };

        let result = intent_queue_update_comment::Entity::insert(active_model)
            .exec(&self.db)
            .await?;
        Ok(result.last_insert_id)
    }

    async fn get_pending_post_intents(&self, limit: u64) -> Result<Vec<PendingPostIntent>> {
        let models = intent_queue_post_comment::Entity::find()
            .filter(
                intent_queue_post_comment::COLUMN
                    .status
                    .eq(IntentStatus::Pending),
            )
            .filter(attempt_due(
                intent_queue_post_comment::Column::NextAttemptAt,
            ))
            .order_by_asc(intent_queue_post_comment::Column::CreatedAt)
            .limit(limit)
            .all(&self.db)
            .await?;

        let mut intents = Vec::new();
        for m in models {
            match serde_json::from_str::<PostCommentIntent>(&m.payload) {
                Ok(intent) => intents.push(PendingPostIntent { id: m.id, intent }),
                Err(e) => warn!(
                    "Skipping corrupt post intent {} (will not block the batch): {:#}",
                    m.id, e
                ),
            }
        }
        Ok(intents)
    }

    async fn get_pending_delete_intents(&self, limit: u64) -> Result<Vec<PendingDeleteIntent>> {
        let models = intent_queue_delete_comment::Entity::find()
            .filter(
                intent_queue_delete_comment::COLUMN
                    .status
                    .eq(IntentStatus::Pending),
            )
            .filter(attempt_due(
                intent_queue_delete_comment::Column::NextAttemptAt,
            ))
            .order_by_asc(intent_queue_delete_comment::Column::CreatedAt)
            .limit(limit)
            .all(&self.db)
            .await?;

        let mut intents = Vec::new();
        for m in models {
            match serde_json::from_str::<DeleteCommentIntent>(&m.payload) {
                Ok(intent) => intents.push(PendingDeleteIntent { id: m.id, intent }),
                Err(e) => warn!(
                    "Skipping corrupt delete intent {} (will not block the batch): {:#}",
                    m.id, e
                ),
            }
        }
        Ok(intents)
    }

    async fn get_pending_update_intents(&self, limit: u64) -> Result<Vec<PendingUpdateIntent>> {
        let models = intent_queue_update_comment::Entity::find()
            .filter(
                intent_queue_update_comment::COLUMN
                    .status
                    .eq(IntentStatus::Pending),
            )
            .filter(attempt_due(
                intent_queue_update_comment::Column::NextAttemptAt,
            ))
            .order_by_asc(intent_queue_update_comment::Column::CreatedAt)
            .limit(limit)
            .all(&self.db)
            .await?;

        let mut intents = Vec::new();
        for m in models {
            let intent = UpdateCommentIntent {
                site_id: m.site_id.into(),
                post_slug: m.post_slug.into(),
                event_id: m.event_id,
                content: m.content,
                author_public_key: m.author_public_key.unwrap_or_default(),
                author_signature: m.author_signature.unwrap_or_default(),
                author_challenge: m.author_challenge.unwrap_or_default(),
            };
            intents.push(PendingUpdateIntent { id: m.id, intent });
        }
        Ok(intents)
    }

    async fn mark_post_intent_waiting_for_sync(
        &self,
        id: i64,
        event_id: &str,
        room_id: &str,
    ) -> Result<()> {
        self.transition_status(
            IntentStatus::WaitingForSync,
            intent_queue_post_comment::Column::Status,
            intent_queue_post_comment::Column::UpdatedAt,
            |query: UpdateMany<intent_queue_post_comment::Entity>| {
                query
                    .col_expr(
                        intent_queue_post_comment::COLUMN.matrix_event_id,
                        sea_orm::sea_query::Expr::value(event_id),
                    )
                    .col_expr(
                        intent_queue_post_comment::COLUMN.room_id,
                        sea_orm::sea_query::Expr::value(room_id),
                    )
                    .filter(intent_queue_post_comment::COLUMN.id.eq(id))
                    // Never regress an already-completed intent: if the
                    // projector closed the loop before this write-back
                    // (push arrived first), keep the completed status.
                    .filter(
                        intent_queue_post_comment::COLUMN
                            .status
                            .eq(IntentStatus::Pending),
                    )
            },
        )
        .await
    }

    async fn mark_post_intent_completed_by_id(&self, id: i64) -> Result<()> {
        self.transition_status(
            IntentStatus::Completed,
            intent_queue_post_comment::Column::Status,
            intent_queue_post_comment::Column::UpdatedAt,
            |query: UpdateMany<intent_queue_post_comment::Entity>| {
                query
                    .filter(intent_queue_post_comment::COLUMN.id.eq(id))
                    // Allow a failed intent to be completed when the
                    // projector later observes its event: failure may have
                    // been a false dead-letter from the timeout pass.
                    .filter(intent_queue_post_comment::COLUMN.status.is_in([
                        IntentStatus::Pending,
                        IntentStatus::WaitingForSync,
                        IntentStatus::Failed,
                    ]))
            },
        )
        .await
    }

    async fn mark_update_intent_waiting_for_sync(&self, id: i64, room_id: &str) -> Result<()> {
        self.transition_status(
            IntentStatus::WaitingForSync,
            intent_queue_update_comment::Column::Status,
            intent_queue_update_comment::Column::UpdatedAt,
            |query: UpdateMany<intent_queue_update_comment::Entity>| {
                query
                    .col_expr(
                        intent_queue_update_comment::COLUMN.room_id,
                        sea_orm::sea_query::Expr::value(room_id),
                    )
                    .filter(intent_queue_update_comment::COLUMN.id.eq(id))
                    // Never regress an already-completed intent (push may have
                    // arrived before this write-back).
                    .filter(
                        intent_queue_update_comment::COLUMN
                            .status
                            .eq(IntentStatus::Pending),
                    )
            },
        )
        .await
    }

    async fn mark_update_intent_completed_by_id(&self, id: i64) -> Result<()> {
        self.transition_status(
            IntentStatus::Completed,
            intent_queue_update_comment::Column::Status,
            intent_queue_update_comment::Column::UpdatedAt,
            |query: UpdateMany<intent_queue_update_comment::Entity>| {
                query
                    .filter(intent_queue_update_comment::COLUMN.id.eq(id))
                    // Never resurrect a failed or already-completed intent.
                    .filter(
                        intent_queue_update_comment::COLUMN
                            .status
                            .is_in([IntentStatus::Pending, IntentStatus::WaitingForSync]),
                    )
            },
        )
        .await
    }

    async fn mark_delete_intent_waiting_for_sync(&self, id: i64, room_id: &str) -> Result<()> {
        self.transition_status(
            IntentStatus::WaitingForSync,
            intent_queue_delete_comment::Column::Status,
            intent_queue_delete_comment::Column::UpdatedAt,
            |query: UpdateMany<intent_queue_delete_comment::Entity>| {
                query
                    .col_expr(
                        intent_queue_delete_comment::COLUMN.room_id,
                        sea_orm::sea_query::Expr::value(room_id),
                    )
                    .filter(intent_queue_delete_comment::COLUMN.id.eq(id))
                    // Never regress an already-completed intent (push may have
                    // arrived before this write-back).
                    .filter(
                        intent_queue_delete_comment::COLUMN
                            .status
                            .eq(IntentStatus::Pending),
                    )
            },
        )
        .await
    }

    async fn record_post_intent_failure(&self, id: i64, error: &str) -> Result<bool> {
        let Some(model) = intent_queue_post_comment::Entity::find_by_id(id)
            .one(&self.db)
            .await?
        else {
            return Ok(false);
        };

        // Completed or dead-lettered intents must not be resurrected.
        if model.status == IntentStatus::Completed || model.status == IntentStatus::Failed {
            return Ok(false);
        }

        if model.retry_count >= MAX_RETRIES {
            let result = intent_queue_post_comment::Entity::update_many()
                .col_expr(
                    intent_queue_post_comment::Column::Status,
                    sea_orm::sea_query::Expr::value(IntentStatus::Failed),
                )
                .col_expr(
                    intent_queue_post_comment::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(chrono::Utc::now()),
                )
                .col_expr(
                    intent_queue_post_comment::Column::LastError,
                    sea_orm::sea_query::Expr::value(error),
                )
                .filter(intent_queue_post_comment::Column::Id.eq(id))
                .filter(intent_queue_post_comment::Column::Status.eq(model.status))
                .filter(intent_queue_post_comment::Column::RetryCount.eq(model.retry_count))
                .exec(&self.db)
                .await?;
            if result.rows_affected == 0 {
                return Ok(false);
            }
            Ok(false)
        } else {
            let next_attempt = chrono::Utc::now() + backoff_after(model.retry_count);
            let result = intent_queue_post_comment::Entity::update_many()
                .col_expr(
                    intent_queue_post_comment::Column::Status,
                    sea_orm::sea_query::Expr::value(IntentStatus::Pending),
                )
                .col_expr(
                    intent_queue_post_comment::Column::RetryCount,
                    sea_orm::sea_query::Expr::value(model.retry_count + 1),
                )
                .col_expr(
                    intent_queue_post_comment::Column::NextAttemptAt,
                    sea_orm::sea_query::Expr::value(next_attempt),
                )
                .col_expr(
                    intent_queue_post_comment::Column::LastError,
                    sea_orm::sea_query::Expr::value(error),
                )
                .col_expr(
                    intent_queue_post_comment::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(chrono::Utc::now()),
                )
                .filter(intent_queue_post_comment::Column::Id.eq(id))
                .filter(intent_queue_post_comment::Column::Status.eq(model.status))
                .filter(intent_queue_post_comment::Column::RetryCount.eq(model.retry_count))
                .exec(&self.db)
                .await?;
            if result.rows_affected == 0 {
                return Ok(false);
            }
            Ok(true)
        }
    }

    async fn record_delete_intent_failure(&self, id: i64, error: &str) -> Result<bool> {
        let Some(model) = intent_queue_delete_comment::Entity::find_by_id(id)
            .one(&self.db)
            .await?
        else {
            return Ok(false);
        };

        // Completed or dead-lettered intents must not be resurrected.
        if model.status == IntentStatus::Completed || model.status == IntentStatus::Failed {
            return Ok(false);
        }

        if model.retry_count >= MAX_RETRIES {
            let result = intent_queue_delete_comment::Entity::update_many()
                .col_expr(
                    intent_queue_delete_comment::Column::Status,
                    sea_orm::sea_query::Expr::value(IntentStatus::Failed),
                )
                .col_expr(
                    intent_queue_delete_comment::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(chrono::Utc::now()),
                )
                .col_expr(
                    intent_queue_delete_comment::Column::LastError,
                    sea_orm::sea_query::Expr::value(error),
                )
                .filter(intent_queue_delete_comment::Column::Id.eq(id))
                .filter(intent_queue_delete_comment::Column::Status.eq(model.status))
                .filter(intent_queue_delete_comment::Column::RetryCount.eq(model.retry_count))
                .exec(&self.db)
                .await?;
            if result.rows_affected == 0 {
                return Ok(false);
            }
            Ok(false)
        } else {
            let next_attempt = chrono::Utc::now() + backoff_after(model.retry_count);
            let result = intent_queue_delete_comment::Entity::update_many()
                .col_expr(
                    intent_queue_delete_comment::Column::Status,
                    sea_orm::sea_query::Expr::value(IntentStatus::Pending),
                )
                .col_expr(
                    intent_queue_delete_comment::Column::RetryCount,
                    sea_orm::sea_query::Expr::value(model.retry_count + 1),
                )
                .col_expr(
                    intent_queue_delete_comment::Column::NextAttemptAt,
                    sea_orm::sea_query::Expr::value(next_attempt),
                )
                .col_expr(
                    intent_queue_delete_comment::Column::LastError,
                    sea_orm::sea_query::Expr::value(error),
                )
                .col_expr(
                    intent_queue_delete_comment::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(chrono::Utc::now()),
                )
                .filter(intent_queue_delete_comment::Column::Id.eq(id))
                .filter(intent_queue_delete_comment::Column::Status.eq(model.status))
                .filter(intent_queue_delete_comment::Column::RetryCount.eq(model.retry_count))
                .exec(&self.db)
                .await?;
            if result.rows_affected == 0 {
                return Ok(false);
            }
            Ok(true)
        }
    }

    async fn record_update_intent_failure(&self, id: i64, error: &str) -> Result<bool> {
        let Some(model) = intent_queue_update_comment::Entity::find_by_id(id)
            .one(&self.db)
            .await?
        else {
            return Ok(false);
        };

        // Completed or dead-lettered intents must not be resurrected.
        if model.status == IntentStatus::Completed || model.status == IntentStatus::Failed {
            return Ok(false);
        }

        if model.retry_count >= MAX_RETRIES {
            let result = intent_queue_update_comment::Entity::update_many()
                .col_expr(
                    intent_queue_update_comment::Column::Status,
                    sea_orm::sea_query::Expr::value(IntentStatus::Failed),
                )
                .col_expr(
                    intent_queue_update_comment::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(chrono::Utc::now()),
                )
                .col_expr(
                    intent_queue_update_comment::Column::LastError,
                    sea_orm::sea_query::Expr::value(error),
                )
                .filter(intent_queue_update_comment::Column::Id.eq(id))
                .filter(intent_queue_update_comment::Column::Status.eq(model.status))
                .filter(intent_queue_update_comment::Column::RetryCount.eq(model.retry_count))
                .exec(&self.db)
                .await?;
            if result.rows_affected == 0 {
                return Ok(false);
            }
            Ok(false)
        } else {
            let next_attempt = chrono::Utc::now() + backoff_after(model.retry_count);
            let result = intent_queue_update_comment::Entity::update_many()
                .col_expr(
                    intent_queue_update_comment::Column::Status,
                    sea_orm::sea_query::Expr::value(IntentStatus::Pending),
                )
                .col_expr(
                    intent_queue_update_comment::Column::RetryCount,
                    sea_orm::sea_query::Expr::value(model.retry_count + 1),
                )
                .col_expr(
                    intent_queue_update_comment::Column::NextAttemptAt,
                    sea_orm::sea_query::Expr::value(next_attempt),
                )
                .col_expr(
                    intent_queue_update_comment::Column::LastError,
                    sea_orm::sea_query::Expr::value(error),
                )
                .col_expr(
                    intent_queue_update_comment::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(chrono::Utc::now()),
                )
                .filter(intent_queue_update_comment::Column::Id.eq(id))
                .filter(intent_queue_update_comment::Column::Status.eq(model.status))
                .filter(intent_queue_update_comment::Column::RetryCount.eq(model.retry_count))
                .exec(&self.db)
                .await?;
            if result.rows_affected == 0 {
                return Ok(false);
            }
            Ok(true)
        }
    }

    async fn mark_post_intent_completed(&self, event_id: &str) -> Result<()> {
        self.transition_status(
            IntentStatus::Completed,
            intent_queue_post_comment::Column::Status,
            intent_queue_post_comment::Column::UpdatedAt,
            |query: UpdateMany<intent_queue_post_comment::Entity>| {
                query.filter(
                    intent_queue_post_comment::COLUMN
                        .matrix_event_id
                        .eq(event_id),
                )
            },
        )
        .await
    }

    async fn get_stuck_post_intents(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
        limit: u64,
    ) -> Result<Vec<StuckPostIntent>> {
        let models = intent_queue_post_comment::Entity::find()
            .filter(
                intent_queue_post_comment::COLUMN
                    .status
                    .eq(IntentStatus::WaitingForSync),
            )
            .filter(intent_queue_post_comment::Column::UpdatedAt.lte(cutoff))
            .filter(
                Condition::any()
                    .add(intent_queue_post_comment::Column::LastTimeoutConfirmationAt.is_null())
                    .add(
                        intent_queue_post_comment::Column::LastTimeoutConfirmationAt.lte(
                            chrono::Utc::now().timestamp_millis()
                                - TIMEOUT_CONFIRMATION_COOLDOWN_MS,
                        ),
                    ),
            )
            .limit(limit)
            .all(&self.db)
            .await?;

        Ok(models
            .into_iter()
            .map(|m| StuckPostIntent {
                id: m.id,
                event_id: m.matrix_event_id.unwrap_or_default(),
                room_id: m.room_id,
            })
            .collect())
    }

    async fn get_stuck_delete_intent_ids(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
        limit: u64,
    ) -> Result<Vec<i64>> {
        let models = intent_queue_delete_comment::Entity::find()
            .filter(
                intent_queue_delete_comment::COLUMN
                    .status
                    .eq(IntentStatus::WaitingForSync),
            )
            .filter(intent_queue_delete_comment::Column::UpdatedAt.lte(cutoff))
            .limit(limit)
            .all(&self.db)
            .await?;

        Ok(models.into_iter().map(|m| m.id).collect())
    }

    async fn get_stuck_update_intent_ids(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
        limit: u64,
    ) -> Result<Vec<i64>> {
        let models = intent_queue_update_comment::Entity::find()
            .filter(
                intent_queue_update_comment::COLUMN
                    .status
                    .eq(IntentStatus::WaitingForSync),
            )
            .filter(intent_queue_update_comment::Column::UpdatedAt.lte(cutoff))
            .limit(limit)
            .all(&self.db)
            .await?;

        Ok(models.into_iter().map(|m| m.id).collect())
    }

    async fn dead_letter_post_intent(&self, id: i64, error: &str) -> Result<()> {
        intent_queue_post_comment::Entity::update_many()
            .col_expr(
                intent_queue_post_comment::Column::Status,
                sea_orm::sea_query::Expr::value(IntentStatus::Failed),
            )
            .col_expr(
                intent_queue_post_comment::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(chrono::Utc::now()),
            )
            .col_expr(
                intent_queue_post_comment::Column::LastError,
                sea_orm::sea_query::Expr::value(error),
            )
            .filter(intent_queue_post_comment::Column::Id.eq(id))
            // Never dead-letter an intent that already completed.
            .filter(
                intent_queue_post_comment::Column::Status
                    .is_in([IntentStatus::Pending, IntentStatus::WaitingForSync]),
            )
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn increment_post_timeout_confirmation(&self, id: i64) -> Result<u32> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.db
            .execute_unprepared(&format!(
                "UPDATE intent_queue_post_comment \
                 SET timeout_confirmations = timeout_confirmations + 1, \
                     last_timeout_confirmation_at = {now_ms} \
                 WHERE id = {id}"
            ))
            .await?;

        let model = intent_queue_post_comment::Entity::find_by_id(id)
            .one(&self.db)
            .await?;
        Ok(model.map(|m| m.timeout_confirmations as u32).unwrap_or(0))
    }

    async fn reset_post_timeout_confirmations(&self, id: i64) -> Result<()> {
        self.db
            .execute_unprepared(&format!(
                "UPDATE intent_queue_post_comment \
                 SET timeout_confirmations = 0, \
                     last_timeout_confirmation_at = NULL \
                 WHERE id = {id}"
            ))
            .await?;
        Ok(())
    }

    async fn increment_post_timeout_error(&self, id: i64) -> Result<u32> {
        self.db
            .execute_unprepared(&format!(
                "UPDATE intent_queue_post_comment \
                 SET timeout_check_errors = timeout_check_errors + 1 \
                 WHERE id = {id}"
            ))
            .await?;
        let model = intent_queue_post_comment::Entity::find_by_id(id)
            .one(&self.db)
            .await?;
        Ok(model.map(|m| m.timeout_check_errors as u32).unwrap_or(0))
    }

    async fn reset_post_timeout_errors(&self, id: i64) -> Result<()> {
        self.db
            .execute_unprepared(&format!(
                "UPDATE intent_queue_post_comment \
                 SET timeout_check_errors = 0 \
                 WHERE id = {id}"
            ))
            .await?;
        Ok(())
    }

    async fn mark_delete_intent_completed(&self, target_event_id: &str) -> Result<()> {
        self.transition_status(
            IntentStatus::Completed,
            intent_queue_delete_comment::Column::Status,
            intent_queue_delete_comment::Column::UpdatedAt,
            |query: UpdateMany<intent_queue_delete_comment::Entity>| {
                query
                    .filter(
                        intent_queue_delete_comment::COLUMN
                            .target_event_id
                            .eq(target_event_id),
                    )
                    // Never resurrect a failed or already-completed intent.
                    .filter(
                        intent_queue_delete_comment::COLUMN
                            .status
                            .is_in([IntentStatus::Pending, IntentStatus::WaitingForSync]),
                    )
            },
        )
        .await
    }

    async fn mark_update_intent_completed(
        &self,
        event_id: &str,
        author_public_key: Option<&str>,
    ) -> Result<()> {
        self.transition_status(
            IntentStatus::Completed,
            intent_queue_update_comment::Column::Status,
            intent_queue_update_comment::Column::UpdatedAt,
            |query: UpdateMany<intent_queue_update_comment::Entity>| {
                let query = query.filter(intent_queue_update_comment::COLUMN.event_id.eq(event_id));
                let query = match author_public_key {
                    Some(key) => query.filter(
                        intent_queue_update_comment::COLUMN
                            .author_public_key
                            .eq(key),
                    ),
                    None => query.filter(
                        intent_queue_update_comment::COLUMN
                            .author_public_key
                            .is_null(),
                    ),
                };
                query
                    // Only close intents that were actually sent; pending rows
                    // must wait for their own `host.curious.cumments.intent_id`
                    // correlation.
                    .filter(
                        intent_queue_update_comment::COLUMN
                            .status
                            .eq(IntentStatus::WaitingForSync),
                    )
            },
        )
        .await
    }
}

#[cfg(test)]
mod backoff_tests {
    use super::*;

    #[test]
    fn backoff_grows_exponentially_and_caps() {
        assert_eq!(backoff_after(0).num_seconds(), 30);
        assert_eq!(backoff_after(1).num_seconds(), 60);
        assert_eq!(backoff_after(2).num_seconds(), 120);
        assert_eq!(backoff_after(5).num_seconds(), 960);
        assert_eq!(backoff_after(6).num_seconds(), 1800);
        assert_eq!(backoff_after(100).num_seconds(), 1800);
    }
}
