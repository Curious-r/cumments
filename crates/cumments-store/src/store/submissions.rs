use super::DbStore;
use crate::entities::{
    active_enums::SubmissionStatus, delete_submissions, idempotency_keys, post_submissions,
    update_submissions,
};
use anyhow::Result;
use async_trait::async_trait;
use cumments_core::commands::{DeleteCommentCommand, PostCommentCommand, UpdateCommentCommand};
use cumments_core::ports::SubmissionStore;
use cumments_core::submissions::{
    IdempotencyInput, IdempotencyOutcome, PendingDeleteSubmission, PendingPostSubmission,
    PendingUpdateSubmission, StuckPostSubmission,
};
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DatabaseBackend, DatabaseTransaction, EntityTrait,
    QueryFilter, QueryOrder, QuerySelect, Set, Statement, TransactionTrait, UpdateMany, Value,
};
use tracing::warn;

/// Maximum failed attempts before an command is marked `failed` (dead-lettered).
const MAX_RETRIES: i64 = 5;
/// Base exponential-backoff delay after the first failure.
const BASE_BACKOFF_SECS: i64 = 30;
/// Upper bound for the backoff delay.
const MAX_BACKOFF_SECS: i64 = 1800;
/// Minimum time between two timeout confirmation passes for the same post
/// command. Matches the reconciler's 60s fallback interval so the three
/// confirmations required before dead-lettering are genuinely spread across
/// reconcile cycles.
const TIMEOUT_CONFIRMATION_COOLDOWN_MS: i64 = 60_000;
/// How long an `Idempotency-Key` stays valid (aligned with Stripe's 24h
/// retention). Expired rows are purged lazily on the next idempotent write.
const IDEMPOTENCY_RETENTION: chrono::Duration = chrono::Duration::hours(24);

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

/// Claims up to `limit` due rows in `table` under one transaction: selects
/// the candidate ids, then flips only rows that are still `pending` to
/// `processing` with the given lease. Returns the claimed ids.
async fn claim_ids(
    db: &sea_orm::DatabaseConnection,
    table: &str,
    limit: u64,
    lease_until: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<i64>> {
    let txn = db.begin().await?;
    let select = format!(
        "SELECT id FROM {table} \
         WHERE status = 'pending' AND (next_attempt_at IS NULL OR next_attempt_at <= ?) \
         ORDER BY created_at ASC LIMIT ?"
    );
    let rows = txn
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            &select,
            vec![chrono::Utc::now().into(), (limit as i64).into()],
        ))
        .await?;
    let ids = rows
        .iter()
        .filter_map(|row| row.try_get_by_index::<i64>(0).ok())
        .collect::<Vec<_>>();
    if ids.is_empty() {
        txn.commit().await?;
        return Ok(ids);
    }

    let placeholders = vec!["?"; ids.len()].join(", ");
    let mut values: Vec<Value> = vec![lease_until.into(), chrono::Utc::now().into()];
    values.extend(ids.iter().map(|id| (*id).into()));
    let update = format!(
        "UPDATE {table} \
         SET status = 'processing', lease_expires_at = ?, updated_at = ? \
         WHERE status = 'pending' AND id IN ({placeholders})"
    );
    txn.execute_raw(Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        &update,
        values,
    ))
    .await?;
    txn.commit().await?;
    Ok(ids)
}

#[async_trait]
impl SubmissionStore for DbStore {
    async fn lookup_idempotency(
        &self,
        idempotency: &IdempotencyInput,
    ) -> Result<Option<IdempotencyOutcome>> {
        let txn = self.db.begin().await?;
        self.purge_expired_idempotency(&txn).await?;
        let outcome = self.existing_idempotency_outcome(&txn, idempotency).await?;
        txn.commit().await?;
        Ok(outcome)
    }

    async fn save_post_submission(&self, command: &PostCommentCommand) -> Result<i64> {
        let payload = serde_json::to_string(command)?;

        let active_model = post_submissions::ActiveModel {
            payload: Set(payload),
            status: Set(SubmissionStatus::Pending),
            retry_count: Set(0),
            timeout_confirmations: Set(0),
            timeout_check_errors: Set(0),
            last_timeout_confirmation_at: Set(None),
            author_public_key: Set(Some(command.author_public_key.clone())),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        };

        let result = post_submissions::Entity::insert(active_model)
            .exec(&self.db)
            .await?;
        Ok(result.last_insert_id)
    }

    async fn save_delete_submission(&self, command: &DeleteCommentCommand) -> Result<i64> {
        let payload = serde_json::to_string(command)?;

        let active_model = delete_submissions::ActiveModel {
            payload: Set(payload),
            status: Set(SubmissionStatus::Pending),
            target_event_id: Set(Some(command.event_id.clone())),
            retry_count: Set(0),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        };

        let result = delete_submissions::Entity::insert(active_model)
            .exec(&self.db)
            .await?;
        Ok(result.last_insert_id)
    }

    async fn save_update_submission(&self, command: &UpdateCommentCommand) -> Result<i64> {
        let active_model = update_submissions::ActiveModel {
            site_id: Set(command.site_id.as_str().to_owned()),
            post_slug: Set(command.post_slug.as_str().to_owned()),
            event_id: Set(command.event_id.clone()),
            content: Set(command.content.clone()),
            author_public_key: Set(Some(command.author_public_key.clone())),
            author_signature: Set(Some(command.author_signature.clone())),
            author_challenge: Set(Some(command.author_challenge.clone())),
            status: Set(SubmissionStatus::Pending),
            retry_count: Set(0),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        };

        let result = update_submissions::Entity::insert(active_model)
            .exec(&self.db)
            .await?;
        Ok(result.last_insert_id)
    }

    async fn save_post_submission_idempotent(
        &self,
        command: &PostCommentCommand,
        idempotency: &IdempotencyInput,
    ) -> Result<IdempotencyOutcome> {
        let txn = self.db.begin().await?;
        self.purge_expired_idempotency(&txn).await?;
        if let Some(outcome) = self.existing_idempotency_outcome(&txn, idempotency).await? {
            return Ok(outcome);
        }

        let payload = serde_json::to_string(command)?;
        let active_model = post_submissions::ActiveModel {
            payload: Set(payload),
            status: Set(SubmissionStatus::Pending),
            retry_count: Set(0),
            timeout_confirmations: Set(0),
            timeout_check_errors: Set(0),
            last_timeout_confirmation_at: Set(None),
            author_public_key: Set(Some(command.author_public_key.clone())),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        };
        let result = post_submissions::Entity::insert(active_model)
            .exec(&txn)
            .await?;
        let submission_id = result.last_insert_id;

        let outcome = self
            .save_idempotency_record(&txn, idempotency, submission_id)
            .await?;
        if let IdempotencyOutcome::Replayed {
            submission_id: existing_id,
        } = &outcome
            && *existing_id != submission_id
        {
            // Another writer won the key race: drop the command this request
            // just queued so only the winner's work is ever processed.
            post_submissions::Entity::delete_by_id(submission_id)
                .exec(&txn)
                .await?;
        }
        if matches!(outcome, IdempotencyOutcome::Reused) {
            // Roll back the duplicate command; invalid reuse is not recorded.
            return Ok(outcome);
        }
        txn.commit().await?;
        Ok(outcome)
    }

    async fn save_delete_submission_idempotent(
        &self,
        command: &DeleteCommentCommand,
        idempotency: &IdempotencyInput,
    ) -> Result<IdempotencyOutcome> {
        let txn = self.db.begin().await?;
        self.purge_expired_idempotency(&txn).await?;
        if let Some(outcome) = self.existing_idempotency_outcome(&txn, idempotency).await? {
            return Ok(outcome);
        }

        let payload = serde_json::to_string(command)?;
        let active_model = delete_submissions::ActiveModel {
            payload: Set(payload),
            status: Set(SubmissionStatus::Pending),
            target_event_id: Set(Some(command.event_id.clone())),
            retry_count: Set(0),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        };
        let result = delete_submissions::Entity::insert(active_model)
            .exec(&txn)
            .await?;
        let submission_id = result.last_insert_id;

        let outcome = self
            .save_idempotency_record(&txn, idempotency, submission_id)
            .await?;
        if let IdempotencyOutcome::Replayed {
            submission_id: existing_id,
        } = &outcome
            && *existing_id != submission_id
        {
            delete_submissions::Entity::delete_by_id(submission_id)
                .exec(&txn)
                .await?;
        }
        if matches!(outcome, IdempotencyOutcome::Reused) {
            return Ok(outcome);
        }
        txn.commit().await?;
        Ok(outcome)
    }

    async fn save_update_submission_idempotent(
        &self,
        command: &UpdateCommentCommand,
        idempotency: &IdempotencyInput,
    ) -> Result<IdempotencyOutcome> {
        let txn = self.db.begin().await?;
        self.purge_expired_idempotency(&txn).await?;
        if let Some(outcome) = self.existing_idempotency_outcome(&txn, idempotency).await? {
            return Ok(outcome);
        }

        let active_model = update_submissions::ActiveModel {
            site_id: Set(command.site_id.as_str().to_owned()),
            post_slug: Set(command.post_slug.as_str().to_owned()),
            event_id: Set(command.event_id.clone()),
            content: Set(command.content.clone()),
            author_public_key: Set(Some(command.author_public_key.clone())),
            author_signature: Set(Some(command.author_signature.clone())),
            author_challenge: Set(Some(command.author_challenge.clone())),
            status: Set(SubmissionStatus::Pending),
            retry_count: Set(0),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        };
        let result = update_submissions::Entity::insert(active_model)
            .exec(&txn)
            .await?;
        let submission_id = result.last_insert_id;

        let outcome = self
            .save_idempotency_record(&txn, idempotency, submission_id)
            .await?;
        if let IdempotencyOutcome::Replayed {
            submission_id: existing_id,
        } = &outcome
            && *existing_id != submission_id
        {
            update_submissions::Entity::delete_by_id(submission_id)
                .exec(&txn)
                .await?;
        }
        if matches!(outcome, IdempotencyOutcome::Reused) {
            return Ok(outcome);
        }
        txn.commit().await?;
        Ok(outcome)
    }

    async fn recover_expired_submission_leases(&self) -> Result<u64> {
        let now = chrono::Utc::now();
        let mut recovered = 0u64;
        recovered += post_submissions::Entity::update_many()
            .col_expr(
                post_submissions::Column::Status,
                sea_orm::sea_query::Expr::value(SubmissionStatus::Pending),
            )
            .col_expr(
                post_submissions::Column::LeaseExpiresAt,
                sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
            )
            .filter(post_submissions::Column::Status.eq(SubmissionStatus::Processing))
            .filter(post_submissions::Column::LeaseExpiresAt.lte(now))
            .exec(&self.db)
            .await?
            .rows_affected;
        recovered += update_submissions::Entity::update_many()
            .col_expr(
                update_submissions::Column::Status,
                sea_orm::sea_query::Expr::value(SubmissionStatus::Pending),
            )
            .col_expr(
                update_submissions::Column::LeaseExpiresAt,
                sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
            )
            .filter(update_submissions::Column::Status.eq(SubmissionStatus::Processing))
            .filter(update_submissions::Column::LeaseExpiresAt.lte(now))
            .exec(&self.db)
            .await?
            .rows_affected;
        recovered += delete_submissions::Entity::update_many()
            .col_expr(
                delete_submissions::Column::Status,
                sea_orm::sea_query::Expr::value(SubmissionStatus::Pending),
            )
            .col_expr(
                delete_submissions::Column::LeaseExpiresAt,
                sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
            )
            .filter(delete_submissions::Column::Status.eq(SubmissionStatus::Processing))
            .filter(delete_submissions::Column::LeaseExpiresAt.lte(now))
            .exec(&self.db)
            .await?
            .rows_affected;
        Ok(recovered)
    }

    async fn claim_pending_post_submissions(
        &self,
        limit: u64,
        lease_until: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<PendingPostSubmission>> {
        let ids = claim_ids(&self.db, "post_submissions", limit, lease_until).await?;
        let models = post_submissions::Entity::find()
            .filter(post_submissions::Column::Id.is_in(ids))
            .all(&self.db)
            .await?;
        let mut submissions = Vec::new();
        for m in models {
            match serde_json::from_str::<PostCommentCommand>(&m.payload) {
                Ok(command) => submissions.push(PendingPostSubmission { id: m.id, command }),
                Err(e) => warn!(
                    "Skipping corrupt post command {} (will not block the batch): {:#}",
                    m.id, e
                ),
            }
        }
        Ok(submissions)
    }

    async fn claim_pending_delete_submissions(
        &self,
        limit: u64,
        lease_until: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<PendingDeleteSubmission>> {
        let ids = claim_ids(&self.db, "delete_submissions", limit, lease_until).await?;
        let models = delete_submissions::Entity::find()
            .filter(delete_submissions::Column::Id.is_in(ids))
            .all(&self.db)
            .await?;
        let mut submissions = Vec::new();
        for m in models {
            match serde_json::from_str::<DeleteCommentCommand>(&m.payload) {
                Ok(command) => submissions.push(PendingDeleteSubmission { id: m.id, command }),
                Err(e) => warn!(
                    "Skipping corrupt delete command {} (will not block the batch): {:#}",
                    m.id, e
                ),
            }
        }
        Ok(submissions)
    }

    async fn claim_pending_update_submissions(
        &self,
        limit: u64,
        lease_until: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<PendingUpdateSubmission>> {
        let ids = claim_ids(&self.db, "update_submissions", limit, lease_until).await?;
        let models = update_submissions::Entity::find()
            .filter(update_submissions::Column::Id.is_in(ids))
            .all(&self.db)
            .await?;
        Ok(models
            .into_iter()
            .map(|m| PendingUpdateSubmission {
                id: m.id,
                command: UpdateCommentCommand {
                    site_id: m.site_id.into(),
                    post_slug: m.post_slug.into(),
                    event_id: m.event_id,
                    content: m.content,
                    author_public_key: m.author_public_key.unwrap_or_default(),
                    author_signature: m.author_signature.unwrap_or_default(),
                    author_challenge: m.author_challenge.unwrap_or_default(),
                },
            })
            .collect())
    }

    async fn mark_post_submission_waiting_for_sync(
        &self,
        id: i64,
        event_id: &str,
        room_id: &str,
    ) -> Result<()> {
        self.transition_status(
            SubmissionStatus::WaitingForSync,
            post_submissions::Column::Status,
            post_submissions::Column::UpdatedAt,
            |query: UpdateMany<post_submissions::Entity>| {
                query
                    .col_expr(
                        post_submissions::COLUMN.matrix_event_id,
                        sea_orm::sea_query::Expr::value(event_id),
                    )
                    .col_expr(
                        post_submissions::COLUMN.room_id,
                        sea_orm::sea_query::Expr::value(room_id),
                    )
                    .col_expr(
                        post_submissions::COLUMN.lease_expires_at,
                        sea_orm::sea_query::Expr::value(
                            Option::<chrono::DateTime<chrono::Utc>>::None,
                        ),
                    )
                    .filter(post_submissions::COLUMN.id.eq(id))
                    // Never regress an already-completed command: if the
                    // projector closed the loop before this write-back
                    // (push arrived first), keep the completed status.
                    .filter(
                        post_submissions::COLUMN
                            .status
                            .is_in([SubmissionStatus::Pending, SubmissionStatus::Processing]),
                    )
            },
        )
        .await
    }

    async fn mark_post_submission_completed_by_id(&self, id: i64) -> Result<()> {
        self.transition_status(
            SubmissionStatus::Completed,
            post_submissions::Column::Status,
            post_submissions::Column::UpdatedAt,
            |query: UpdateMany<post_submissions::Entity>| {
                query
                    .filter(post_submissions::COLUMN.id.eq(id))
                    // Allow a failed command to be completed when the
                    // projector later observes its event: failure may have
                    // been a false dead-letter from the timeout pass.
                    .filter(post_submissions::COLUMN.status.is_in([
                        SubmissionStatus::Pending,
                        SubmissionStatus::Processing,
                        SubmissionStatus::WaitingForSync,
                        SubmissionStatus::Failed,
                    ]))
            },
        )
        .await
    }

    async fn mark_update_submission_waiting_for_sync(&self, id: i64, room_id: &str) -> Result<()> {
        self.transition_status(
            SubmissionStatus::WaitingForSync,
            update_submissions::Column::Status,
            update_submissions::Column::UpdatedAt,
            |query: UpdateMany<update_submissions::Entity>| {
                query
                    .col_expr(
                        update_submissions::COLUMN.room_id,
                        sea_orm::sea_query::Expr::value(room_id),
                    )
                    .col_expr(
                        update_submissions::COLUMN.lease_expires_at,
                        sea_orm::sea_query::Expr::value(
                            Option::<chrono::DateTime<chrono::Utc>>::None,
                        ),
                    )
                    .filter(update_submissions::COLUMN.id.eq(id))
                    // Never regress an already-completed command (push may have
                    // arrived before this write-back).
                    .filter(
                        update_submissions::COLUMN
                            .status
                            .is_in([SubmissionStatus::Pending, SubmissionStatus::Processing]),
                    )
            },
        )
        .await
    }

    async fn mark_update_submission_completed_by_id(&self, id: i64) -> Result<()> {
        self.transition_status(
            SubmissionStatus::Completed,
            update_submissions::Column::Status,
            update_submissions::Column::UpdatedAt,
            |query: UpdateMany<update_submissions::Entity>| {
                query
                    .filter(update_submissions::COLUMN.id.eq(id))
                    // Never resurrect a failed or already-completed command.
                    .filter(update_submissions::COLUMN.status.is_in([
                        SubmissionStatus::Pending,
                        SubmissionStatus::Processing,
                        SubmissionStatus::WaitingForSync,
                    ]))
            },
        )
        .await
    }

    async fn mark_delete_submission_waiting_for_sync(&self, id: i64, room_id: &str) -> Result<()> {
        self.transition_status(
            SubmissionStatus::WaitingForSync,
            delete_submissions::Column::Status,
            delete_submissions::Column::UpdatedAt,
            |query: UpdateMany<delete_submissions::Entity>| {
                query
                    .col_expr(
                        delete_submissions::COLUMN.room_id,
                        sea_orm::sea_query::Expr::value(room_id),
                    )
                    .col_expr(
                        delete_submissions::COLUMN.lease_expires_at,
                        sea_orm::sea_query::Expr::value(
                            Option::<chrono::DateTime<chrono::Utc>>::None,
                        ),
                    )
                    .filter(delete_submissions::COLUMN.id.eq(id))
                    // Never regress an already-completed command (push may have
                    // arrived before this write-back).
                    .filter(
                        delete_submissions::COLUMN
                            .status
                            .is_in([SubmissionStatus::Pending, SubmissionStatus::Processing]),
                    )
            },
        )
        .await
    }

    async fn record_post_submission_failure(&self, id: i64, error: &str) -> Result<bool> {
        let Some(model) = post_submissions::Entity::find_by_id(id)
            .one(&self.db)
            .await?
        else {
            return Ok(false);
        };

        // Completed or dead-lettered submissions must not be resurrected.
        if model.status == SubmissionStatus::Completed || model.status == SubmissionStatus::Failed {
            return Ok(false);
        }

        if model.retry_count >= MAX_RETRIES {
            let result = post_submissions::Entity::update_many()
                .col_expr(
                    post_submissions::Column::Status,
                    sea_orm::sea_query::Expr::value(SubmissionStatus::Failed),
                )
                .col_expr(
                    post_submissions::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(chrono::Utc::now()),
                )
                .col_expr(
                    post_submissions::Column::LastError,
                    sea_orm::sea_query::Expr::value(error),
                )
                .filter(post_submissions::Column::Id.eq(id))
                .filter(post_submissions::Column::Status.eq(model.status))
                .filter(post_submissions::Column::RetryCount.eq(model.retry_count))
                .exec(&self.db)
                .await?;
            if result.rows_affected == 0 {
                return Ok(false);
            }
            Ok(false)
        } else {
            let next_attempt = chrono::Utc::now() + backoff_after(model.retry_count);
            let result = post_submissions::Entity::update_many()
                .col_expr(
                    post_submissions::Column::Status,
                    sea_orm::sea_query::Expr::value(SubmissionStatus::Pending),
                )
                .col_expr(
                    post_submissions::Column::RetryCount,
                    sea_orm::sea_query::Expr::value(model.retry_count + 1),
                )
                .col_expr(
                    post_submissions::Column::NextAttemptAt,
                    sea_orm::sea_query::Expr::value(next_attempt),
                )
                .col_expr(
                    post_submissions::Column::LastError,
                    sea_orm::sea_query::Expr::value(error),
                )
                .col_expr(
                    post_submissions::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(chrono::Utc::now()),
                )
                .filter(post_submissions::Column::Id.eq(id))
                .filter(post_submissions::Column::Status.eq(model.status))
                .filter(post_submissions::Column::RetryCount.eq(model.retry_count))
                .exec(&self.db)
                .await?;
            if result.rows_affected == 0 {
                return Ok(false);
            }
            Ok(true)
        }
    }

    async fn record_delete_submission_failure(&self, id: i64, error: &str) -> Result<bool> {
        let Some(model) = delete_submissions::Entity::find_by_id(id)
            .one(&self.db)
            .await?
        else {
            return Ok(false);
        };

        // Completed or dead-lettered submissions must not be resurrected.
        if model.status == SubmissionStatus::Completed || model.status == SubmissionStatus::Failed {
            return Ok(false);
        }

        if model.retry_count >= MAX_RETRIES {
            let result = delete_submissions::Entity::update_many()
                .col_expr(
                    delete_submissions::Column::Status,
                    sea_orm::sea_query::Expr::value(SubmissionStatus::Failed),
                )
                .col_expr(
                    delete_submissions::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(chrono::Utc::now()),
                )
                .col_expr(
                    delete_submissions::Column::LastError,
                    sea_orm::sea_query::Expr::value(error),
                )
                .filter(delete_submissions::Column::Id.eq(id))
                .filter(delete_submissions::Column::Status.eq(model.status))
                .filter(delete_submissions::Column::RetryCount.eq(model.retry_count))
                .exec(&self.db)
                .await?;
            if result.rows_affected == 0 {
                return Ok(false);
            }
            Ok(false)
        } else {
            let next_attempt = chrono::Utc::now() + backoff_after(model.retry_count);
            let result = delete_submissions::Entity::update_many()
                .col_expr(
                    delete_submissions::Column::Status,
                    sea_orm::sea_query::Expr::value(SubmissionStatus::Pending),
                )
                .col_expr(
                    delete_submissions::Column::RetryCount,
                    sea_orm::sea_query::Expr::value(model.retry_count + 1),
                )
                .col_expr(
                    delete_submissions::Column::NextAttemptAt,
                    sea_orm::sea_query::Expr::value(next_attempt),
                )
                .col_expr(
                    delete_submissions::Column::LastError,
                    sea_orm::sea_query::Expr::value(error),
                )
                .col_expr(
                    delete_submissions::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(chrono::Utc::now()),
                )
                .filter(delete_submissions::Column::Id.eq(id))
                .filter(delete_submissions::Column::Status.eq(model.status))
                .filter(delete_submissions::Column::RetryCount.eq(model.retry_count))
                .exec(&self.db)
                .await?;
            if result.rows_affected == 0 {
                return Ok(false);
            }
            Ok(true)
        }
    }

    async fn record_update_submission_failure(&self, id: i64, error: &str) -> Result<bool> {
        let Some(model) = update_submissions::Entity::find_by_id(id)
            .one(&self.db)
            .await?
        else {
            return Ok(false);
        };

        // Completed or dead-lettered submissions must not be resurrected.
        if model.status == SubmissionStatus::Completed || model.status == SubmissionStatus::Failed {
            return Ok(false);
        }

        if model.retry_count >= MAX_RETRIES {
            let result = update_submissions::Entity::update_many()
                .col_expr(
                    update_submissions::Column::Status,
                    sea_orm::sea_query::Expr::value(SubmissionStatus::Failed),
                )
                .col_expr(
                    update_submissions::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(chrono::Utc::now()),
                )
                .col_expr(
                    update_submissions::Column::LastError,
                    sea_orm::sea_query::Expr::value(error),
                )
                .filter(update_submissions::Column::Id.eq(id))
                .filter(update_submissions::Column::Status.eq(model.status))
                .filter(update_submissions::Column::RetryCount.eq(model.retry_count))
                .exec(&self.db)
                .await?;
            if result.rows_affected == 0 {
                return Ok(false);
            }
            Ok(false)
        } else {
            let next_attempt = chrono::Utc::now() + backoff_after(model.retry_count);
            let result = update_submissions::Entity::update_many()
                .col_expr(
                    update_submissions::Column::Status,
                    sea_orm::sea_query::Expr::value(SubmissionStatus::Pending),
                )
                .col_expr(
                    update_submissions::Column::RetryCount,
                    sea_orm::sea_query::Expr::value(model.retry_count + 1),
                )
                .col_expr(
                    update_submissions::Column::NextAttemptAt,
                    sea_orm::sea_query::Expr::value(next_attempt),
                )
                .col_expr(
                    update_submissions::Column::LastError,
                    sea_orm::sea_query::Expr::value(error),
                )
                .col_expr(
                    update_submissions::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(chrono::Utc::now()),
                )
                .filter(update_submissions::Column::Id.eq(id))
                .filter(update_submissions::Column::Status.eq(model.status))
                .filter(update_submissions::Column::RetryCount.eq(model.retry_count))
                .exec(&self.db)
                .await?;
            if result.rows_affected == 0 {
                return Ok(false);
            }
            Ok(true)
        }
    }

    async fn mark_post_submission_completed(&self, event_id: &str) -> Result<()> {
        self.transition_status(
            SubmissionStatus::Completed,
            post_submissions::Column::Status,
            post_submissions::Column::UpdatedAt,
            |query: UpdateMany<post_submissions::Entity>| {
                query.filter(post_submissions::COLUMN.matrix_event_id.eq(event_id))
            },
        )
        .await
    }

    async fn get_stuck_post_submissions(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
        limit: u64,
    ) -> Result<Vec<StuckPostSubmission>> {
        let models = post_submissions::Entity::find()
            .filter(
                post_submissions::COLUMN
                    .status
                    .eq(SubmissionStatus::WaitingForSync),
            )
            .filter(post_submissions::Column::UpdatedAt.lte(cutoff))
            .filter(
                Condition::any()
                    .add(post_submissions::Column::LastTimeoutConfirmationAt.is_null())
                    .add(post_submissions::Column::LastTimeoutConfirmationAt.lte(
                        chrono::Utc::now().timestamp_millis() - TIMEOUT_CONFIRMATION_COOLDOWN_MS,
                    )),
            )
            .limit(limit)
            .all(&self.db)
            .await?;

        Ok(models
            .into_iter()
            .map(|m| StuckPostSubmission {
                id: m.id,
                event_id: m.matrix_event_id.unwrap_or_default(),
                room_id: m.room_id,
            })
            .collect())
    }

    async fn get_stuck_delete_submission_ids(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
        limit: u64,
    ) -> Result<Vec<i64>> {
        let models = delete_submissions::Entity::find()
            .filter(
                delete_submissions::COLUMN
                    .status
                    .eq(SubmissionStatus::WaitingForSync),
            )
            .filter(delete_submissions::Column::UpdatedAt.lte(cutoff))
            .limit(limit)
            .all(&self.db)
            .await?;

        Ok(models.into_iter().map(|m| m.id).collect())
    }

    async fn get_stuck_update_submission_ids(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
        limit: u64,
    ) -> Result<Vec<i64>> {
        let models = update_submissions::Entity::find()
            .filter(
                update_submissions::COLUMN
                    .status
                    .eq(SubmissionStatus::WaitingForSync),
            )
            .filter(update_submissions::Column::UpdatedAt.lte(cutoff))
            .limit(limit)
            .all(&self.db)
            .await?;

        Ok(models.into_iter().map(|m| m.id).collect())
    }

    async fn dead_letter_post_submission(&self, id: i64, error: &str) -> Result<()> {
        post_submissions::Entity::update_many()
            .col_expr(
                post_submissions::Column::Status,
                sea_orm::sea_query::Expr::value(SubmissionStatus::Failed),
            )
            .col_expr(
                post_submissions::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(chrono::Utc::now()),
            )
            .col_expr(
                post_submissions::Column::LastError,
                sea_orm::sea_query::Expr::value(error),
            )
            .filter(post_submissions::Column::Id.eq(id))
            // Never dead-letter an command that already completed.
            .filter(
                post_submissions::Column::Status
                    .is_in([SubmissionStatus::Pending, SubmissionStatus::WaitingForSync]),
            )
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn increment_post_timeout_confirmation(&self, id: i64) -> Result<u32> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.db
            .execute_unprepared(&format!(
                "UPDATE post_submissions \
                 SET timeout_confirmations = timeout_confirmations + 1, \
                     last_timeout_confirmation_at = {now_ms} \
                 WHERE id = {id}"
            ))
            .await?;

        let model = post_submissions::Entity::find_by_id(id)
            .one(&self.db)
            .await?;
        Ok(model.map(|m| m.timeout_confirmations as u32).unwrap_or(0))
    }

    async fn reset_post_timeout_confirmations(&self, id: i64) -> Result<()> {
        self.db
            .execute_unprepared(&format!(
                "UPDATE post_submissions \
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
                "UPDATE post_submissions \
                 SET timeout_check_errors = timeout_check_errors + 1 \
                 WHERE id = {id}"
            ))
            .await?;
        let model = post_submissions::Entity::find_by_id(id)
            .one(&self.db)
            .await?;
        Ok(model.map(|m| m.timeout_check_errors as u32).unwrap_or(0))
    }

    async fn reset_post_timeout_errors(&self, id: i64) -> Result<()> {
        self.db
            .execute_unprepared(&format!(
                "UPDATE post_submissions \
                 SET timeout_check_errors = 0 \
                 WHERE id = {id}"
            ))
            .await?;
        Ok(())
    }

    async fn mark_delete_submission_completed(&self, target_event_id: &str) -> Result<()> {
        self.transition_status(
            SubmissionStatus::Completed,
            delete_submissions::Column::Status,
            delete_submissions::Column::UpdatedAt,
            |query: UpdateMany<delete_submissions::Entity>| {
                query
                    .filter(
                        delete_submissions::COLUMN
                            .target_event_id
                            .eq(target_event_id),
                    )
                    // Never resurrect a failed or already-completed command.
                    .filter(delete_submissions::COLUMN.status.is_in([
                        SubmissionStatus::Pending,
                        SubmissionStatus::Processing,
                        SubmissionStatus::WaitingForSync,
                    ]))
            },
        )
        .await
    }

    async fn mark_update_submission_completed(
        &self,
        event_id: &str,
        author_public_key: Option<&str>,
    ) -> Result<()> {
        self.transition_status(
            SubmissionStatus::Completed,
            update_submissions::Column::Status,
            update_submissions::Column::UpdatedAt,
            |query: UpdateMany<update_submissions::Entity>| {
                let query = query.filter(update_submissions::COLUMN.event_id.eq(event_id));
                let query = match author_public_key {
                    Some(key) => query.filter(update_submissions::COLUMN.author_public_key.eq(key)),
                    None => query.filter(update_submissions::COLUMN.author_public_key.is_null()),
                };
                query
                    // Only close submissions that were actually sent; pending rows
                    // must wait for their own `host.curious.cumments.submission_id`
                    // correlation.
                    .filter(update_submissions::COLUMN.status.is_in([
                        SubmissionStatus::Processing,
                        SubmissionStatus::WaitingForSync,
                    ]))
            },
        )
        .await
    }
}

impl DbStore {
    /// Deletes idempotency rows older than the 24-hour retention window.
    ///
    /// Runs inside the same transaction as the write so a stale key can be
    /// reused immediately without a separate cleanup pass.
    async fn purge_expired_idempotency(&self, txn: &DatabaseTransaction) -> Result<()> {
        idempotency_keys::Entity::delete_many()
            .filter(
                idempotency_keys::Column::CreatedAt.lt(chrono::Utc::now() - IDEMPOTENCY_RETENTION),
            )
            .exec(txn)
            .await?;
        Ok(())
    }

    /// Returns the stored outcome when this key was already used.
    async fn existing_idempotency_outcome(
        &self,
        txn: &DatabaseTransaction,
        idempotency: &IdempotencyInput,
    ) -> Result<Option<IdempotencyOutcome>> {
        let row = idempotency_keys::Entity::find()
            .filter(idempotency_keys::Column::AuthorPublicKey.eq(&idempotency.author_public_key))
            .filter(idempotency_keys::Column::IdempotencyKey.eq(&idempotency.key))
            .one(txn)
            .await?;

        let Some(row) = row else {
            return Ok(None);
        };
        if row.request_fingerprint == idempotency.request_fingerprint {
            Ok(Some(IdempotencyOutcome::Replayed {
                submission_id: row.submission_id,
            }))
        } else {
            Ok(Some(IdempotencyOutcome::Reused))
        }
    }

    /// Inserts the idempotency record for a freshly queued command.
    ///
    /// The insert ignores unique-constraint conflicts; when two writers race
    /// for the same key, the loser reads the winner's row and reports replay
    /// or reuse instead of queueing a duplicate command.
    async fn save_idempotency_record(
        &self,
        txn: &DatabaseTransaction,
        idempotency: &IdempotencyInput,
        submission_id: i64,
    ) -> Result<IdempotencyOutcome> {
        let backend = txn.get_database_backend();
        let sql = if backend == DatabaseBackend::Sqlite {
            "INSERT OR IGNORE INTO idempotency_keys \
             (author_public_key, idempotency_key, request_fingerprint, submission_id, created_at) \
             VALUES (?, ?, ?, ?, ?)"
        } else {
            "INSERT INTO idempotency_keys \
             (author_public_key, idempotency_key, request_fingerprint, submission_id, created_at) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT (author_public_key, idempotency_key) DO NOTHING"
        };
        let inserted = txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                sql,
                vec![
                    Value::from(idempotency.author_public_key.clone()),
                    Value::from(idempotency.key.clone()),
                    Value::from(idempotency.request_fingerprint.clone()),
                    Value::from(submission_id),
                    Value::from(chrono::Utc::now()),
                ],
            ))
            .await?;
        if inserted.rows_affected() > 0 {
            return Ok(IdempotencyOutcome::Accepted { submission_id });
        }

        let existing = idempotency_keys::Entity::find()
            .filter(idempotency_keys::Column::AuthorPublicKey.eq(&idempotency.author_public_key))
            .filter(idempotency_keys::Column::IdempotencyKey.eq(&idempotency.key))
            .one(txn)
            .await?
            .ok_or_else(|| anyhow::anyhow!("idempotency insert conflicted but no row exists"))?;

        if existing.request_fingerprint == idempotency.request_fingerprint {
            Ok(IdempotencyOutcome::Replayed {
                submission_id: existing.submission_id,
            })
        } else {
            Ok(IdempotencyOutcome::Reused)
        }
    }
}

impl DbStore {
    /// Peek at up to `limit` due pending post submissions without claiming
    /// them. Inspection/testing helper; production passes claim instead.
    pub async fn get_pending_post_submissions(
        &self,
        limit: u64,
    ) -> Result<Vec<PendingPostSubmission>> {
        let models = post_submissions::Entity::find()
            .filter(
                post_submissions::COLUMN
                    .status
                    .eq(SubmissionStatus::Pending),
            )
            .filter(attempt_due(post_submissions::Column::NextAttemptAt))
            .order_by_asc(post_submissions::Column::CreatedAt)
            .limit(limit)
            .all(&self.db)
            .await?;
        let mut submissions = Vec::new();
        for m in models {
            match serde_json::from_str::<PostCommentCommand>(&m.payload) {
                Ok(command) => submissions.push(PendingPostSubmission { id: m.id, command }),
                Err(e) => warn!(
                    "Skipping corrupt post command {} (will not block the batch): {:#}",
                    m.id, e
                ),
            }
        }
        Ok(submissions)
    }

    /// Peek at up to `limit` due pending delete submissions without claiming
    /// them. Inspection/testing helper.
    pub async fn get_pending_delete_submissions(
        &self,
        limit: u64,
    ) -> Result<Vec<PendingDeleteSubmission>> {
        let models = delete_submissions::Entity::find()
            .filter(
                delete_submissions::COLUMN
                    .status
                    .eq(SubmissionStatus::Pending),
            )
            .filter(attempt_due(delete_submissions::Column::NextAttemptAt))
            .order_by_asc(delete_submissions::Column::CreatedAt)
            .limit(limit)
            .all(&self.db)
            .await?;
        let mut submissions = Vec::new();
        for m in models {
            match serde_json::from_str::<DeleteCommentCommand>(&m.payload) {
                Ok(command) => submissions.push(PendingDeleteSubmission { id: m.id, command }),
                Err(e) => warn!(
                    "Skipping corrupt delete command {} (will not block the batch): {:#}",
                    m.id, e
                ),
            }
        }
        Ok(submissions)
    }

    /// Peek at up to `limit` due pending update submissions without claiming
    /// them. Inspection/testing helper.
    pub async fn get_pending_update_submissions(
        &self,
        limit: u64,
    ) -> Result<Vec<PendingUpdateSubmission>> {
        let models = update_submissions::Entity::find()
            .filter(
                update_submissions::COLUMN
                    .status
                    .eq(SubmissionStatus::Pending),
            )
            .filter(attempt_due(update_submissions::Column::NextAttemptAt))
            .order_by_asc(update_submissions::Column::CreatedAt)
            .limit(limit)
            .all(&self.db)
            .await?;
        Ok(models
            .into_iter()
            .map(|m| PendingUpdateSubmission {
                id: m.id,
                command: UpdateCommentCommand {
                    site_id: m.site_id.into(),
                    post_slug: m.post_slug.into(),
                    event_id: m.event_id,
                    content: m.content,
                    author_public_key: m.author_public_key.unwrap_or_default(),
                    author_signature: m.author_signature.unwrap_or_default(),
                    author_challenge: m.author_challenge.unwrap_or_default(),
                },
            })
            .collect())
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
