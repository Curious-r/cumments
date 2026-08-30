use super::DbStore;
use super::is_unique_violation;
use crate::entities::active_enums::SubmissionStatus;
use crate::entities::{
    backfill_tombstones, delete_submissions, media_upload_idempotency, media_uploads,
    message_revisions, messages, poll_response_events, post_submissions,
    processed_appservice_transactions, reactions, room_members, update_submissions,
};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use cumments_core::media_upload::{
    MEDIA_UPLOAD_IDEMPOTENCY_RETENTION, MediaUploadIdempotency, MediaUploadIdempotencyInput,
    MediaUploadIdempotencyOutcome,
};
use cumments_core::models::{
    AuthorKind, AuthorSnapshot, Content, EditProjectionOutcome, Message, MessagePage,
    MessageRedactionOutcome, MessageRevision, MessageSaveOutcome, MessageStatus, PageSlug,
    PollResponseSummary, PollVote, Reaction, ReactionSummary, SiteId, SubmissionCompletion,
    UnknownContent,
};
use cumments_core::ports::{AppServiceTxnStore, MessageStore, ProjectionSink};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, EntityTrait, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use std::collections::{HashMap, HashSet};
/// Internal reaction aggregate for Phase 1 (count + bounded sender sample).
/// `selected_senders` is the deterministic top-N sample; not yet exposed via
/// public API. See `misc/design/reaction-reactors.md` §3-7.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionAggregate {
    pub key: String,
    pub count: i64,
    /// Ordered top-5 sender_mxids for this (message,key), sorted by
    /// representative `origin_server_ts DESC, event_id DESC, sender ASC`.
    /// Internal only — never serialized to `ReactionSummary` in Phase 1.
    pub selected_senders: Vec<String>,
}

fn content_to_json(content: &Content) -> String {
    serde_json::to_string(content).expect("content serializes")
}

fn content_from_json(raw: &str) -> Content {
    serde_json::from_str(raw).unwrap_or_else(|_| {
        tracing::warn!(raw = %raw, "falling back to unknown content for dirty content_json");
        Content::Unknown(UnknownContent {
            fallback: None,
            raw: serde_json::Value::Null,
        })
    })
}

fn unknown_content() -> Content {
    Content::Unknown(UnknownContent {
        fallback: None,
        raw: serde_json::Value::Null,
    })
}

async fn insert_message_if_absent<C: ConnectionTrait>(
    conn: &C,
    message: &Message,
) -> Result<MessageSaveOutcome> {
    let now = chrono::Utc::now();
    let active_model = messages::ActiveModel {
        event_id: Set(message.event_id.clone()),
        room_id: Set(message.room_id.clone()),
        site_id: Set(message.site_id.clone()),
        page_slug: Set(message.page_slug.clone()),
        sender_mxid: Set(message.sender_mxid.clone()),
        author_kind: Set(message.author.kind.as_str().to_string()),
        author_display_name: Set(message.author.display_name.clone()),
        author_avatar_url: Set(message.author.avatar_url.clone()),
        author_public_key: Set(message.author.public_key.clone()),
        content_json: Set(content_to_json(&message.content)),
        original_content_json: Set(content_to_json(&message.content)),
        matrix_event_type: Set(message.matrix_event_type.clone()),
        raw_content_json: Set(
            serde_json::to_string(&message.raw_content).unwrap_or_else(|_| "null".to_string())
        ),
        timestamp: Set(message.timestamp),
        reply_to: Set(message.reply_to.clone()),
        thread_root: Set(message.thread_root.clone()),
        status: Set(message.status.as_str().to_string()),
        redacted_at: Set(message.redacted_at),
        redacted_by: Set(message.redacted_by.clone()),
        submission_id: Set(message.submission_id),
        last_edit_ts: Set(message.edited_at.map(|t| t.timestamp_millis())),
        last_edit_event_id: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };

    match messages::Entity::insert(active_model)
        .on_conflict(
            sea_orm::sea_query::OnConflict::column(messages::Column::EventId)
                .do_nothing()
                .to_owned(),
        )
        .exec(conn)
        .await
    {
        Ok(_) => Ok(MessageSaveOutcome::Inserted),
        Err(sea_orm::DbErr::RecordNotInserted) => Ok(MessageSaveOutcome::AlreadyProjected),
        Err(error) => Err(error.into()),
    }
}

async fn apply_edit_on<C: ConnectionTrait>(
    conn: &C,
    message: &Message,
    revision: &MessageRevision,
) -> Result<EditProjectionOutcome> {
    let edit_ts = revision.edited_at.timestamp_millis();

    if message_revisions::Entity::find()
        .filter(
            message_revisions::COLUMN
                .event_id
                .eq(revision.event_id.clone()),
        )
        .one(conn)
        .await?
        .is_some()
    {
        return Ok(EditProjectionOutcome::AlreadyKnown);
    }

    let parent = messages::Entity::find()
        .filter(messages::COLUMN.event_id.eq(message.event_id.clone()))
        .filter(messages::COLUMN.room_id.eq(message.room_id.clone()))
        .filter(messages::COLUMN.status.eq(MessageStatus::Active.as_str()))
        .one(conn)
        .await?;
    let Some(parent) = parent else {
        return Ok(EditProjectionOutcome::Rejected);
    };

    let revision_model = message_revisions::ActiveModel {
        event_id: Set(revision.event_id.clone()),
        message_event_id: Set(message.event_id.clone()),
        content_json: Set(content_to_json(&revision.content)),
        edited_at: Set(revision.edited_at),
        editor_mxid: Set(revision.editor_mxid.clone()),
        redacted_at: Set(None),
        redacted_by: Set(None),
        created_at: Set(chrono::Utc::now()),
        ..Default::default()
    };
    message_revisions::Entity::insert(revision_model)
        .exec(conn)
        .await?;

    let current = message_revisions::Entity::find()
        .filter(
            message_revisions::COLUMN
                .message_event_id
                .eq(message.event_id.clone()),
        )
        .filter(message_revisions::COLUMN.redacted_at.is_null())
        .order_by_desc(message_revisions::Column::EditedAt)
        .order_by_desc(message_revisions::Column::EventId)
        .one(conn)
        .await?;
    let Some(current) = current else {
        return Ok(EditProjectionOutcome::Rejected);
    };

    if current.event_id != revision.event_id {
        // The valid replacement remains stored as a fact, but it does not
        // become the public view until a newer revision is redacted.
        return Ok(EditProjectionOutcome::Superseded);
    }

    let mut active: messages::ActiveModel = parent.into();
    active.content_json = Set(content_to_json(&message.content));
    active.last_edit_ts = Set(Some(edit_ts));
    active.last_edit_event_id = Set(Some(revision.event_id.clone()));
    active.updated_at = Set(chrono::Utc::now());
    active.update(conn).await?;
    Ok(EditProjectionOutcome::AppliedCurrent)
}

async fn redact_message_on<C: ConnectionTrait>(
    conn: &C,
    event_id: &str,
    room_id: &str,
    redacted_at: chrono::DateTime<chrono::Utc>,
    redacted_by: &str,
) -> Result<MessageRedactionOutcome> {
    // Redaction is a projection rewrite, not just a lifecycle flag: the
    // homeserver strips the original event, so the read model must do the
    // same. Revisions are removed in the same transaction because each one
    // contains an earlier displayable version of the deleted comment.
    let existing = messages::Entity::find()
        .filter(messages::COLUMN.event_id.eq(event_id))
        .one(conn)
        .await?;
    let Some(existing) = existing else {
        return Ok(MessageRedactionOutcome::Rejected);
    };
    if existing.room_id != room_id {
        return Ok(MessageRedactionOutcome::Rejected);
    }
    if existing.status == MessageStatus::Redacted.as_str() {
        return Ok(MessageRedactionOutcome::AlreadyRedacted);
    }

    messages::Entity::update_many()
        .col_expr(
            messages::Column::ContentJson,
            sea_orm::sea_query::Expr::value(content_to_json(&Content::redacted())),
        )
        .col_expr(
            messages::Column::OriginalContentJson,
            sea_orm::sea_query::Expr::value(content_to_json(&Content::redacted())),
        )
        .col_expr(
            messages::Column::RawContentJson,
            sea_orm::sea_query::Expr::value("{}".to_owned()),
        )
        .col_expr(
            messages::Column::Status,
            sea_orm::sea_query::Expr::value(MessageStatus::Redacted.as_str()),
        )
        .col_expr(
            messages::Column::RedactedAt,
            sea_orm::sea_query::Expr::value(Some(redacted_at)),
        )
        .col_expr(
            messages::Column::RedactedBy,
            sea_orm::sea_query::Expr::value(Some(redacted_by.to_owned())),
        )
        .col_expr(
            messages::Column::LastEditTs,
            sea_orm::sea_query::Expr::value(Option::<i64>::None),
        )
        .col_expr(
            messages::Column::LastEditEventId,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            messages::Column::ReplyTo,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            messages::Column::ThreadRoot,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            messages::Column::SubmissionId,
            sea_orm::sea_query::Expr::value(Option::<i64>::None),
        )
        .col_expr(
            messages::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(chrono::Utc::now()),
        )
        .filter(messages::COLUMN.event_id.eq(event_id))
        .filter(messages::COLUMN.room_id.eq(room_id))
        .exec(conn)
        .await?;

    message_revisions::Entity::delete_many()
        .filter(message_revisions::COLUMN.message_event_id.eq(event_id))
        .exec(conn)
        .await?;
    Ok(MessageRedactionOutcome::Redacted)
}

#[async_trait]
impl ProjectionSink for DbStore {
    async fn save_message_unit(
        &self,
        message: &Message,
        completion: SubmissionCompletion,
    ) -> Result<MessageSaveOutcome> {
        let txn = self.db.begin().await?;
        let outcome = insert_message_if_absent(&txn, message).await?;
        if matches!(
            outcome,
            MessageSaveOutcome::Inserted | MessageSaveOutcome::AlreadyProjected
        ) {
            complete_post(&txn, &completion).await?;
        }
        txn.commit().await?;
        Ok(outcome)
    }

    async fn apply_edit_unit(
        &self,
        message: &Message,
        revision: &MessageRevision,
        completion: SubmissionCompletion,
    ) -> Result<EditProjectionOutcome> {
        let txn = self.db.begin().await?;
        let outcome = apply_edit_on(&txn, message, revision).await?;
        if matches!(outcome, EditProjectionOutcome::AlreadyKnown) {
            if matches!(
                completion,
                SubmissionCompletion::UpdateById(_) | SubmissionCompletion::UpdateByEvent { .. }
            ) {
                complete_update(&txn, &completion).await?;
            }
            txn.commit().await?;
            return Ok(outcome);
        }
        if matches!(outcome, EditProjectionOutcome::Rejected) {
            txn.rollback().await?;
            return Ok(outcome);
        }
        if matches!(outcome, EditProjectionOutcome::AppliedCurrent)
            || (matches!(outcome, EditProjectionOutcome::Superseded)
                && !matches!(completion, SubmissionCompletion::None))
        {
            complete_update(&txn, &completion).await?;
        }
        txn.commit().await?;
        Ok(outcome)
    }

    async fn redact_message_unit(
        &self,
        event_id: &str,
        room_id: &str,
        redacted_at: chrono::DateTime<chrono::Utc>,
        redacted_by: &str,
        redaction_event_id: &str,
    ) -> Result<MessageRedactionOutcome> {
        let txn = self.db.begin().await?;
        let outcome = redact_message_on(&txn, event_id, room_id, redacted_at, redacted_by).await?;
        if matches!(outcome, MessageRedactionOutcome::AlreadyRedacted) {
            record_backfill_tombstone_on(&txn, event_id, room_id, redaction_event_id).await?;
            close_delete_submission(&txn, event_id).await?;
            txn.commit().await?;
            return Ok(MessageRedactionOutcome::AlreadyRedacted);
        }
        if matches!(outcome, MessageRedactionOutcome::Rejected) {
            txn.rollback().await?;
            return Ok(outcome);
        }
        record_backfill_tombstone_on(&txn, event_id, room_id, redaction_event_id).await?;
        close_delete_submission(&txn, event_id).await?;
        txn.commit().await?;
        Ok(MessageRedactionOutcome::Redacted)
    }
}

async fn complete_post<C: ConnectionTrait>(
    conn: &C,
    completion: &SubmissionCompletion,
) -> Result<()> {
    match completion {
        SubmissionCompletion::None => {}
        SubmissionCompletion::PostById(id) => {
            post_submissions::Entity::update_many()
                .col_expr(
                    post_submissions::Column::Status,
                    sea_orm::sea_query::Expr::value(SubmissionStatus::Completed),
                )
                .col_expr(
                    post_submissions::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(chrono::Utc::now()),
                )
                .filter(post_submissions::COLUMN.id.eq(*id))
                .filter(post_submissions::COLUMN.status.is_in([
                    SubmissionStatus::Pending,
                    SubmissionStatus::Processing,
                    SubmissionStatus::WaitingForSync,
                    SubmissionStatus::Failed,
                ]))
                .exec(conn)
                .await?;
        }
        SubmissionCompletion::PostByEvent(event_id) => {
            post_submissions::Entity::update_many()
                .col_expr(
                    post_submissions::Column::Status,
                    sea_orm::sea_query::Expr::value(SubmissionStatus::Completed),
                )
                .col_expr(
                    post_submissions::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(chrono::Utc::now()),
                )
                .filter(
                    post_submissions::COLUMN
                        .matrix_event_id
                        .eq(event_id.clone()),
                )
                .exec(conn)
                .await?;
        }
        _ => {}
    }
    Ok(())
}

async fn complete_update<C: ConnectionTrait>(
    conn: &C,
    completion: &SubmissionCompletion,
) -> Result<()> {
    let (scope, allowed_statuses) = match completion {
        SubmissionCompletion::UpdateById(id) => (
            Condition::all().add(update_submissions::COLUMN.id.eq(*id)),
            vec![
                SubmissionStatus::Pending,
                SubmissionStatus::Processing,
                SubmissionStatus::WaitingForSync,
            ],
        ),
        SubmissionCompletion::UpdateByEvent {
            target_event_id,
            author_public_key,
        } => {
            let mut condition =
                Condition::all().add(update_submissions::COLUMN.event_id.eq(target_event_id));
            condition = condition.add(match author_public_key {
                Some(key) => update_submissions::COLUMN.author_public_key.eq(key.clone()),
                None => update_submissions::COLUMN.author_public_key.is_null(),
            });
            (
                condition,
                vec![
                    SubmissionStatus::Processing,
                    SubmissionStatus::WaitingForSync,
                ],
            )
        }
        _ => return Ok(()),
    };
    let query = update_submissions::Entity::update_many()
        .col_expr(
            update_submissions::Column::Status,
            sea_orm::sea_query::Expr::value(SubmissionStatus::Completed),
        )
        .col_expr(
            update_submissions::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(chrono::Utc::now()),
        );
    query
        .filter(scope)
        .filter(update_submissions::COLUMN.status.is_in(allowed_statuses))
        .exec(conn)
        .await?;
    Ok(())
}

async fn close_delete_submission<C: ConnectionTrait>(conn: &C, event_id: &str) -> Result<()> {
    delete_submissions::Entity::update_many()
        .col_expr(
            delete_submissions::Column::Status,
            sea_orm::sea_query::Expr::value(SubmissionStatus::Completed),
        )
        .col_expr(
            delete_submissions::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(chrono::Utc::now()),
        )
        .filter(delete_submissions::COLUMN.target_event_id.eq(event_id))
        .filter(delete_submissions::COLUMN.status.is_in([
            SubmissionStatus::Pending,
            SubmissionStatus::Processing,
            SubmissionStatus::WaitingForSync,
        ]))
        .exec(conn)
        .await?;
    Ok(())
}

async fn record_backfill_tombstone_on<C: ConnectionTrait>(
    conn: &C,
    event_id: &str,
    room_id: &str,
    redaction_event_id: &str,
) -> Result<()> {
    let active_model = backfill_tombstones::ActiveModel {
        event_id: Set(event_id.to_owned()),
        room_id: Set(room_id.to_owned()),
        redaction_event_id: Set(redaction_event_id.to_owned()),
        created_at: Set(chrono::Utc::now()),
    };
    backfill_tombstones::Entity::insert(active_model)
        .on_conflict(
            sea_orm::sea_query::OnConflict::column(backfill_tombstones::Column::EventId)
                .update_column(backfill_tombstones::Column::RoomId)
                .update_column(backfill_tombstones::Column::RedactionEventId)
                .update_column(backfill_tombstones::Column::CreatedAt)
                .to_owned(),
        )
        .exec(conn)
        .await?;
    Ok(())
}

fn message_from_model(model: messages::Model) -> Message {
    let kind = if model.author_kind == "matrix" {
        AuthorKind::Matrix
    } else {
        AuthorKind::Visitor
    };
    let event_id_for_warn = model.event_id.clone();
    let status_raw_for_warn = model.status.clone();
    let raw_content_for_warn = model.raw_content_json.clone();
    let status = model.status.parse().unwrap_or_else(|_| {
        tracing::warn!(
            event_id = %event_id_for_warn,
            raw_status = %status_raw_for_warn,
            "falling back to active status for unknown status value"
        );
        MessageStatus::Active
    });
    // Defense in depth for rows written by an old binary or before migration.
    let (content, raw_content, edited_at, reply_to, thread_root, submission_id) =
        if status == MessageStatus::Redacted {
            (
                Content::redacted(),
                serde_json::json!({}),
                None,
                None,
                None,
                None,
            )
        } else {
            (
                content_from_json(&model.content_json),
                serde_json::from_str(&model.raw_content_json).unwrap_or_else(|_| {
                    tracing::warn!(
                        event_id = %event_id_for_warn,
                        raw = %raw_content_for_warn,
                        "falling back to Null for dirty raw_content_json"
                    );
                    serde_json::Value::Null
                }),
                model
                    .last_edit_ts
                    .and_then(chrono::DateTime::from_timestamp_millis),
                model.reply_to,
                model.thread_root,
                model.submission_id,
            )
        };
    Message {
        event_id: model.event_id,
        site_id: model.site_id,
        page_slug: model.page_slug,
        author: AuthorSnapshot {
            kind,
            display_name: model.author_display_name,
            avatar_url: model.author_avatar_url,
            public_key: model.author_public_key,
            mxid: if kind == AuthorKind::Matrix {
                Some(model.sender_mxid.clone())
            } else {
                None
            },
        },
        content,
        matrix_event_type: model.matrix_event_type,
        timestamp: model.timestamp,
        edited_at,
        reply_to,
        thread_root,
        submission_id,
        status,
        redacted_at: model.redacted_at,
        redacted_by: model.redacted_by,
        reactions: Vec::new(),
        room_id: model.room_id,
        sender_mxid: model.sender_mxid,
        raw_content,
    }
}

#[async_trait]
impl AppServiceTxnStore for DbStore {
    async fn has_processed_txn(&self, txn_id: &str) -> Result<bool> {
        Ok(
            processed_appservice_transactions::Entity::find_by_id(txn_id.to_string())
                .one(&self.db)
                .await?
                .is_some(),
        )
    }

    async fn mark_processed_txn(&self, txn_id: &str) -> Result<()> {
        let now = chrono::Utc::now();
        processed_appservice_transactions::Entity::insert(
            processed_appservice_transactions::ActiveModel {
                txn_id: Set(txn_id.to_string()),
                processed_at: Set(now),
            },
        )
        .on_conflict(
            sea_orm::sea_query::OnConflict::column(
                processed_appservice_transactions::Column::TxnId,
            )
            .do_nothing()
            .to_owned(),
        )
        .exec(&self.db)
        .await?;

        self.db
            .execute_unprepared(
                r"DELETE FROM processed_appservice_transactions
                  WHERE txn_id NOT IN (
                      SELECT txn_id FROM processed_appservice_transactions
                      ORDER BY processed_at DESC, txn_id DESC
                      LIMIT 10000
                  )",
            )
            .await?;
        Ok(())
    }
}

#[async_trait]
impl MessageStore for DbStore {
    async fn get_message(&self, event_id: &str) -> Result<Option<Message>> {
        let model = messages::Entity::find()
            .filter(messages::COLUMN.event_id.eq(event_id))
            .one(&self.db)
            .await?;
        match model {
            Some(model) => Ok(Some(self.hydrate(message_from_model(model)).await?)),
            None => Ok(None),
        }
    }

    async fn get_messages(
        &self,
        site_id: &SiteId,
        page_slug: &PageSlug,
        limit: i64,
        offset: i64,
    ) -> Result<MessagePage> {
        let site_id_str = site_id.as_str();
        let page_slug_str = page_slug.as_str();

        let query = messages::Entity::find()
            .filter(messages::COLUMN.site_id.eq(site_id_str))
            .filter(messages::COLUMN.page_slug.eq(page_slug_str))
            .order_by_desc(messages::Column::Timestamp)
            .order_by_asc(messages::Column::EventId);

        let total = query.clone().count(&self.db).await?;
        if limit <= 0 {
            return Ok(MessagePage {
                items: Vec::new(),
                total: total as i64,
            });
        }

        let models = query
            .offset(offset.max(0) as u64)
            .limit(limit as u64)
            .all(&self.db)
            .await?;

        let event_ids: Vec<String> = models.iter().map(|model| model.event_id.clone()).collect();
        let mut items: Vec<Message> = models.into_iter().map(message_from_model).collect();
        self.hydrate_batch(&mut items, &event_ids).await?;
        Ok(MessagePage {
            items,
            total: total as i64,
        })
    }

    async fn save_message(&self, message: &Message) -> Result<MessageSaveOutcome> {
        insert_message_if_absent(&self.db, message).await
    }

    async fn apply_edit(
        &self,
        message: &Message,
        revision: &MessageRevision,
    ) -> Result<EditProjectionOutcome> {
        let txn = self.db.begin().await?;
        let outcome = apply_edit_on(&txn, message, revision).await?;
        if matches!(outcome, EditProjectionOutcome::Rejected) {
            txn.rollback().await?;
            return Ok(outcome);
        }
        txn.commit().await?;
        Ok(outcome)
    }

    async fn redact_message(
        &self,
        event_id: &str,
        room_id: &str,
        redacted_at: chrono::DateTime<chrono::Utc>,
        redacted_by: &str,
    ) -> Result<MessageRedactionOutcome> {
        let txn = self.db.begin().await?;
        let outcome = redact_message_on(&txn, event_id, room_id, redacted_at, redacted_by).await?;
        if matches!(outcome, MessageRedactionOutcome::Rejected) {
            txn.rollback().await?;
            return Ok(outcome);
        }
        txn.commit().await?;
        Ok(outcome)
    }

    async fn get_message_revision(&self, event_id: &str) -> Result<Option<MessageRevision>> {
        let model = message_revisions::Entity::find()
            .filter(message_revisions::COLUMN.event_id.eq(event_id))
            .one(&self.db)
            .await?;
        Ok(model.map(|m| MessageRevision {
            event_id: m.event_id,
            message_event_id: m.message_event_id,
            content: content_from_json(&m.content_json),
            edited_at: m.edited_at,
            editor_mxid: m.editor_mxid,
            redacted_at: m.redacted_at,
        }))
    }

    async fn redact_message_revision(
        &self,
        event_id: &str,
        room_id: &str,
        redacted_at: chrono::DateTime<chrono::Utc>,
        redacted_by: &str,
    ) -> Result<bool> {
        let txn = self.db.begin().await?;
        let revision = message_revisions::Entity::find()
            .filter(message_revisions::COLUMN.event_id.eq(event_id))
            .one(&txn)
            .await?;
        let Some(revision) = revision else {
            txn.rollback().await?;
            return Ok(false);
        };

        let parent = messages::Entity::find()
            .filter(
                messages::COLUMN
                    .event_id
                    .eq(revision.message_event_id.clone()),
            )
            .filter(messages::COLUMN.room_id.eq(room_id))
            .filter(messages::COLUMN.status.eq(MessageStatus::Active.as_str()))
            .one(&txn)
            .await?;
        let Some(parent) = parent else {
            txn.rollback().await?;
            return Ok(false);
        };

        message_revisions::Entity::update_many()
            .col_expr(
                message_revisions::Column::RedactedAt,
                sea_orm::sea_query::Expr::value(Some(redacted_at)),
            )
            .col_expr(
                message_revisions::Column::ContentJson,
                sea_orm::sea_query::Expr::value(content_to_json(&Content::redacted())),
            )
            .col_expr(
                message_revisions::Column::RedactedBy,
                sea_orm::sea_query::Expr::value(Some(redacted_by.to_owned())),
            )
            .filter(message_revisions::COLUMN.event_id.eq(event_id))
            .exec(&txn)
            .await?;

        let current = message_revisions::Entity::find()
            .filter(
                message_revisions::COLUMN
                    .message_event_id
                    .eq(parent.event_id.clone()),
            )
            .filter(message_revisions::COLUMN.redacted_at.is_null())
            .order_by_desc(message_revisions::Column::EditedAt)
            .order_by_desc(message_revisions::Column::EventId)
            .one(&txn)
            .await?;
        let original_content_json = parent.original_content_json.clone();
        let mut active: messages::ActiveModel = parent.into();
        match current {
            Some(current) => {
                let content: Content = serde_json::from_str(&current.content_json)
                    .unwrap_or_else(|_| unknown_content());
                active.content_json = Set(content_to_json(&content));
                active.last_edit_ts = Set(Some(current.edited_at.timestamp_millis()));
                active.last_edit_event_id = Set(Some(current.event_id));
            }
            None => {
                let original: Content =
                    serde_json::from_str(&original_content_json).unwrap_or(unknown_content());
                active.content_json = Set(content_to_json(&original));
                active.last_edit_ts = Set(None);
                active.last_edit_event_id = Set(None);
            }
        }
        active.updated_at = Set(chrono::Utc::now());
        active.update(&txn).await?;
        txn.commit().await?;
        Ok(true)
    }

    async fn get_author_display_name(&self, event_id: &str) -> Result<Option<Option<String>>> {
        let Some(model) = messages::Entity::find()
            .filter(messages::COLUMN.event_id.eq(event_id))
            .one(&self.db)
            .await?
        else {
            return Ok(None);
        };
        // Edits maintain the author's current display name (live profile),
        // falling back to the stored projection when the member left.
        if let Some(member) = room_members::Entity::find()
            .filter(room_members::Column::RoomId.eq(&model.room_id))
            .filter(room_members::Column::UserId.eq(&model.sender_mxid))
            .filter(room_members::Column::Membership.eq("join"))
            .one(&self.db)
            .await?
        {
            return Ok(Some(member.display_name));
        }
        Ok(Some(model.author_display_name))
    }

    async fn get_author_public_key(&self, event_id: &str) -> Result<Option<String>> {
        let model = messages::Entity::find()
            .filter(messages::COLUMN.event_id.eq(event_id))
            .one(&self.db)
            .await?;
        Ok(model.and_then(|m| m.author_public_key))
    }

    async fn save_reaction(&self, reaction: &Reaction) -> Result<()> {
        let active_model = reactions::ActiveModel {
            event_id: Set(reaction.event_id.clone()),
            message_event_id: Set(reaction.message_event_id.clone()),
            sender_mxid: Set(reaction.sender_mxid.clone()),
            key: Set(reaction.key.clone()),
            origin_server_ts: Set(reaction.origin_server_ts),
            redacted_at: Set(reaction.redacted_at),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        };
        reactions::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(reactions::Column::EventId)
                    .update_columns([
                        reactions::Column::MessageEventId,
                        reactions::Column::SenderMxid,
                        reactions::Column::Key,
                        reactions::Column::OriginServerTs,
                    ])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn get_reaction(&self, event_id: &str) -> Result<Option<Reaction>> {
        let model = reactions::Entity::find()
            .filter(reactions::Column::EventId.eq(event_id))
            .one(&self.db)
            .await?;
        Ok(model.map(|m| Reaction {
            event_id: m.event_id,
            message_event_id: m.message_event_id,
            sender_mxid: m.sender_mxid,
            key: m.key,
            origin_server_ts: m.origin_server_ts,
            redacted_at: m.redacted_at,
        }))
    }

    async fn redact_reaction(
        &self,
        event_id: &str,
        redacted_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        let result = reactions::Entity::update_many()
            .col_expr(
                reactions::Column::RedactedAt,
                sea_orm::sea_query::Expr::value(Some(redacted_at)),
            )
            .filter(reactions::COLUMN.event_id.eq(event_id))
            .exec(&self.db)
            .await?;
        Ok(result.rows_affected > 0)
    }

    async fn find_reaction_by_sender_and_key(
        &self,
        message_event_id: &str,
        sender_mxid: &str,
        key: &str,
    ) -> Result<Option<Reaction>> {
        let model = reactions::Entity::find()
            .filter(reactions::Column::MessageEventId.eq(message_event_id))
            .filter(reactions::Column::SenderMxid.eq(sender_mxid))
            .filter(reactions::Column::Key.eq(key))
            .filter(reactions::Column::RedactedAt.is_null())
            .one(&self.db)
            .await?;
        Ok(model.map(|m| Reaction {
            event_id: m.event_id,
            message_event_id: m.message_event_id,
            sender_mxid: m.sender_mxid,
            key: m.key,
            origin_server_ts: m.origin_server_ts,
            redacted_at: m.redacted_at,
        }))
    }

    async fn find_reaction_keys_by_sender(
        &self,
        message_event_ids: &[String],
        sender_mxid: &str,
    ) -> Result<std::collections::HashMap<String, std::collections::HashSet<String>>> {
        use std::collections::{HashMap, HashSet};
        if message_event_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = reactions::Entity::find()
            .filter(reactions::Column::MessageEventId.is_in(message_event_ids.iter().cloned()))
            .filter(reactions::Column::SenderMxid.eq(sender_mxid))
            .filter(reactions::Column::RedactedAt.is_null())
            .all(&self.db)
            .await?;
        let mut map: HashMap<String, HashSet<String>> = HashMap::new();
        for row in rows {
            map.entry(row.message_event_id).or_default().insert(row.key);
        }
        Ok(map)
    }

    async fn save_poll_vote(&self, vote: &PollVote) -> Result<()> {
        self.save_poll_vote_with_selections(vote, &[], None).await
    }

    async fn save_poll_vote_with_selections(
        &self,
        vote: &PollVote,
        answer_ids: &[String],
        spoiled_reason: Option<&str>,
    ) -> Result<()> {
        let txn = self.db.begin().await?;
        if poll_response_events::Entity::find()
            .filter(
                poll_response_events::COLUMN
                    .event_id
                    .eq(vote.event_id.clone()),
            )
            .one(&txn)
            .await?
            .is_some()
        {
            txn.commit().await?;
            return Ok(());
        }

        let active_model = poll_response_events::ActiveModel {
            event_id: Set(vote.event_id.clone()),
            poll_message_id: Set(vote.poll_message_id.clone()),
            sender_mxid: Set(vote.sender_mxid.clone()),
            option_index: Set(vote.option_index),
            answer_ids_json: Set(
                serde_json::to_string(answer_ids).unwrap_or_else(|_| "[]".to_owned())
            ),
            spoiled_reason: Set(spoiled_reason.map(str::to_owned)),
            origin_server_ts: Set(vote.origin_server_ts),
            redacted_at: Set(None),
            redacted_by: Set(None),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        };
        poll_response_events::Entity::insert(active_model)
            .exec(&txn)
            .await?;
        txn.commit().await?;
        Ok(())
    }

    async fn get_poll_vote_by_event(&self, event_id: &str) -> Result<Option<PollVote>> {
        let model = poll_response_events::Entity::find()
            .filter(poll_response_events::COLUMN.event_id.eq(event_id))
            .one(&self.db)
            .await?;
        Ok(model.map(|m| PollVote {
            event_id: m.event_id,
            poll_message_id: m.poll_message_id,
            sender_mxid: m.sender_mxid,
            option_index: m.option_index,
            origin_server_ts: m.origin_server_ts,
        }))
    }

    async fn redact_poll_vote(
        &self,
        event_id: &str,
        redacted_at: chrono::DateTime<chrono::Utc>,
        redacted_by: &str,
    ) -> Result<bool> {
        let result = poll_response_events::Entity::update_many()
            .col_expr(
                poll_response_events::Column::RedactedAt,
                sea_orm::sea_query::Expr::value(Some(redacted_at)),
            )
            .col_expr(
                poll_response_events::Column::OptionIndex,
                sea_orm::sea_query::Expr::value(Option::<i64>::None),
            )
            .col_expr(
                poll_response_events::Column::AnswerIdsJson,
                sea_orm::sea_query::Expr::value("[]"),
            )
            .col_expr(
                poll_response_events::Column::SpoiledReason,
                sea_orm::sea_query::Expr::value(Option::<String>::None),
            )
            .col_expr(
                poll_response_events::Column::RedactedBy,
                sea_orm::sea_query::Expr::value(Some(redacted_by.to_owned())),
            )
            .filter(poll_response_events::Column::EventId.eq(event_id))
            .exec(&self.db)
            .await?;
        Ok(result.rows_affected > 0)
    }

    async fn record_backfill_tombstone(
        &self,
        event_id: &str,
        room_id: &str,
        redaction_event_id: &str,
    ) -> Result<()> {
        let active_model = backfill_tombstones::ActiveModel {
            event_id: Set(event_id.to_owned()),
            room_id: Set(room_id.to_owned()),
            redaction_event_id: Set(redaction_event_id.to_owned()),
            created_at: Set(chrono::Utc::now()),
        };
        backfill_tombstones::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(backfill_tombstones::Column::EventId)
                    .update_column(backfill_tombstones::Column::RoomId)
                    .update_column(backfill_tombstones::Column::RedactionEventId)
                    .update_column(backfill_tombstones::Column::CreatedAt)
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn has_backfill_tombstone(&self, event_id: &str, room_id: &str) -> Result<bool> {
        let found = backfill_tombstones::Entity::find()
            .filter(backfill_tombstones::COLUMN.event_id.eq(event_id))
            .filter(backfill_tombstones::COLUMN.room_id.eq(room_id))
            .one(&self.db)
            .await?;
        Ok(found.is_some())
    }

    async fn record_media_upload(
        &self,
        mxc_url: &str,
        author_public_key: &str,
        site_id: &str,
        page_slug: Option<&str>,
    ) -> Result<()> {
        let now = chrono::Utc::now();
        let model = media_uploads::ActiveModel {
            mxc_url: Set(mxc_url.to_owned()),
            author_public_key: Set(author_public_key.to_owned()),
            site_id: Set(site_id.to_owned()),
            page_slug: Set(page_slug.map(str::to_string)),
            used_at: Set(None),
            submission_id: Set(None),
            created_at: Set(now),
            ..Default::default()
        };
        media_uploads::Entity::insert(model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(media_uploads::Column::MxcUrl)
                    .update_columns([
                        media_uploads::Column::AuthorPublicKey,
                        media_uploads::Column::SiteId,
                        media_uploads::Column::PageSlug,
                        media_uploads::Column::UsedAt,
                        media_uploads::Column::SubmissionId,
                    ])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn media_upload_owned_by(
        &self,
        mxc_url: &str,
        author_public_key: &str,
        site_id: &str,
        page_slug: &str,
    ) -> Result<bool> {
        let found = media_uploads::Entity::find()
            .filter(media_uploads::Column::MxcUrl.eq(mxc_url))
            .filter(media_uploads::Column::AuthorPublicKey.eq(author_public_key))
            .filter(media_uploads::Column::SiteId.eq(site_id))
            .filter(media_uploads::Column::PageSlug.eq(page_slug))
            .one(&self.db)
            .await?;
        Ok(found.is_some())
    }

    async fn mark_media_used(&self, mxc_url: &str) -> Result<()> {
        media_uploads::Entity::update_many()
            .col_expr(
                media_uploads::Column::UsedAt,
                sea_orm::sea_query::Expr::value(Some(chrono::Utc::now())),
            )
            .filter(media_uploads::Column::MxcUrl.eq(mxc_url))
            .filter(media_uploads::Column::UsedAt.is_null())
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn list_unused_media_before(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<String>> {
        // Media bound to a pending/processing/waiting submission must survive
        // the 24h orphan window: the submission may still send it.
        let active_submissions: Vec<i64> = post_submissions::Entity::find()
            .select_only()
            .column(post_submissions::Column::Id)
            .filter(post_submissions::Column::Status.is_in([
                "pending",
                "processing",
                "waiting_for_sync",
            ]))
            .into_tuple()
            .all(&self.db)
            .await?;
        let rows = media_uploads::Entity::find()
            .filter(media_uploads::Column::UsedAt.is_null())
            .filter(media_uploads::Column::CreatedAt.lt(cutoff))
            .all(&self.db)
            .await?
            .into_iter()
            .filter(|row| {
                row.submission_id
                    .is_none_or(|id| !active_submissions.contains(&id))
            })
            .map(|row| row.mxc_url)
            .collect();
        Ok(rows)
    }

    async fn delete_media_upload(&self, mxc_url: &str) -> Result<()> {
        media_uploads::Entity::delete_many()
            .filter(media_uploads::Column::MxcUrl.eq(mxc_url))
            .exec(&self.db)
            .await?;
        media_upload_idempotency::Entity::delete_many()
            .filter(media_upload_idempotency::Column::MxcUrl.eq(mxc_url))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn list_media_urls_for_site(&self, site_id: &str) -> Result<Vec<String>> {
        let rows = media_uploads::Entity::find()
            .filter(media_uploads::Column::SiteId.eq(site_id))
            .all(&self.db)
            .await?;
        Ok(rows.into_iter().map(|row| row.mxc_url).collect())
    }

    async fn find_media_upload_idempotency(
        &self,
        author_public_key: &str,
        idempotency_key: &str,
    ) -> Result<Option<MediaUploadIdempotency>> {
        let cutoff = chrono::Utc::now() - MEDIA_UPLOAD_IDEMPOTENCY_RETENTION;
        let row = media_upload_idempotency::Entity::find()
            .filter(media_upload_idempotency::Column::AuthorPublicKey.eq(author_public_key))
            .filter(media_upload_idempotency::Column::IdempotencyKey.eq(idempotency_key))
            .filter(media_upload_idempotency::Column::CreatedAt.gte(cutoff))
            .one(&self.db)
            .await?;
        Ok(row.map(|row| MediaUploadIdempotency {
            request_fingerprint: row.request_fingerprint,
            mxc_url: row.mxc_url,
            created_at: row.created_at,
        }))
    }

    async fn save_media_upload_idempotent(
        &self,
        mxc_url: &str,
        author_public_key: &str,
        site_id: &str,
        page_slug: Option<&str>,
        idempotency: &MediaUploadIdempotencyInput,
    ) -> Result<MediaUploadIdempotencyOutcome> {
        let now = chrono::Utc::now();
        let cutoff = now - MEDIA_UPLOAD_IDEMPOTENCY_RETENTION;
        let txn = self.db.begin().await?;

        // Expired rows for this key are purged first so the key can be reused.
        media_upload_idempotency::Entity::delete_many()
            .filter(media_upload_idempotency::Column::AuthorPublicKey.eq(author_public_key))
            .filter(media_upload_idempotency::Column::IdempotencyKey.eq(&idempotency.key))
            .filter(media_upload_idempotency::Column::CreatedAt.lt(cutoff))
            .exec(&txn)
            .await?;

        if let Some(existing) = media_upload_idempotency::Entity::find()
            .filter(media_upload_idempotency::Column::AuthorPublicKey.eq(author_public_key))
            .filter(media_upload_idempotency::Column::IdempotencyKey.eq(&idempotency.key))
            .filter(media_upload_idempotency::Column::CreatedAt.gte(cutoff))
            .one(&txn)
            .await?
        {
            txn.rollback().await?;
            if existing.request_fingerprint == idempotency.request_fingerprint {
                return Ok(MediaUploadIdempotencyOutcome::Replayed {
                    mxc_url: existing.mxc_url,
                });
            }
            return Ok(MediaUploadIdempotencyOutcome::Reused);
        }

        let upload_model = media_uploads::ActiveModel {
            mxc_url: Set(mxc_url.to_owned()),
            author_public_key: Set(author_public_key.to_owned()),
            site_id: Set(site_id.to_owned()),
            page_slug: Set(page_slug.map(str::to_string)),
            used_at: Set(None),
            created_at: Set(now),
            ..Default::default()
        };
        media_uploads::Entity::insert(upload_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(media_uploads::Column::MxcUrl)
                    .update_columns([
                        media_uploads::Column::AuthorPublicKey,
                        media_uploads::Column::SiteId,
                        media_uploads::Column::PageSlug,
                        media_uploads::Column::UsedAt,
                    ])
                    .to_owned(),
            )
            .exec(&txn)
            .await?;

        let idempotency_model = media_upload_idempotency::ActiveModel {
            author_public_key: Set(author_public_key.to_owned()),
            idempotency_key: Set(idempotency.key.clone()),
            request_fingerprint: Set(idempotency.request_fingerprint.clone()),
            mxc_url: Set(mxc_url.to_owned()),
            created_at: Set(now),
            ..Default::default()
        };
        match media_upload_idempotency::Entity::insert(idempotency_model)
            .exec(&txn)
            .await
        {
            Ok(_) => {
                txn.commit().await?;
                Ok(MediaUploadIdempotencyOutcome::Created {
                    mxc_url: mxc_url.to_owned(),
                })
            }
            Err(e) if is_unique_violation(&e) => {
                txn.rollback().await?;
                let winner = media_upload_idempotency::Entity::find()
                    .filter(media_upload_idempotency::Column::AuthorPublicKey.eq(author_public_key))
                    .filter(media_upload_idempotency::Column::IdempotencyKey.eq(&idempotency.key))
                    .filter(media_upload_idempotency::Column::CreatedAt.gte(cutoff))
                    .one(&self.db)
                    .await?;
                match winner {
                    Some(winner)
                        if winner.request_fingerprint == idempotency.request_fingerprint =>
                    {
                        Ok(MediaUploadIdempotencyOutcome::Replayed {
                            mxc_url: winner.mxc_url,
                        })
                    }
                    Some(_) => Ok(MediaUploadIdempotencyOutcome::Reused),
                    None => Err(anyhow!(
                        "media upload idempotency conflict without a winner row"
                    )),
                }
            }
            Err(e) => {
                txn.rollback().await?;
                Err(e.into())
            }
        }
    }
}

impl DbStore {
    /// Hydrate a message with its aggregated reactions and poll responses.
    async fn hydrate(&self, mut message: Message) -> Result<Message> {
        let mut reactions = self
            .reaction_summary_map(std::slice::from_ref(&message.event_id))
            .await?;
        message.reactions = reactions.remove(&message.event_id).unwrap_or_default();
        if let Content::Poll(poll) = &mut message.content {
            let mut responses = self
                .poll_response_summary_map(std::slice::from_ref(&message.event_id))
                .await?;
            poll.responses = responses.remove(&message.event_id).unwrap_or_default();
        }
        self.sanitize_relations(std::slice::from_mut(&mut message))
            .await?;
        self.enrich_author_profiles(std::slice::from_mut(&mut message))
            .await?;
        Ok(message)
    }

    /// Batch variant of [`Self::hydrate`]: one query per annotation table for
    /// a whole page instead of two queries per message.
    async fn hydrate_batch(&self, messages: &mut [Message], event_ids: &[String]) -> Result<()> {
        let mut reactions = self.reaction_summary_map(event_ids).await?;
        let mut responses = self.poll_response_summary_map(event_ids).await?;
        for message in messages.iter_mut() {
            message.reactions = reactions.remove(&message.event_id).unwrap_or_default();
            if let Content::Poll(poll) = &mut message.content {
                poll.responses = responses.remove(&message.event_id).unwrap_or_default();
            }
        }
        self.sanitize_relations(messages).await?;
        self.enrich_author_profiles(messages).await?;
        Ok(())
    }

    /// Relations to deleted or missing messages are not part of the public
    /// contract. Clear them in the derived view while preserving the child's
    /// immutable Matrix fact.
    async fn sanitize_relations(&self, messages: &mut [Message]) -> Result<()> {
        let targets: Vec<String> = messages
            .iter()
            .filter_map(|message| {
                message
                    .reply_to
                    .clone()
                    .or_else(|| message.thread_root.clone())
            })
            .collect();
        if targets.is_empty() {
            return Ok(());
        }

        let active_targets: HashSet<String> = messages::Entity::find()
            .filter(messages::COLUMN.event_id.is_in(targets))
            .filter(messages::COLUMN.status.eq(MessageStatus::Active.as_str()))
            .all(&self.db)
            .await?
            .into_iter()
            .map(|model| model.event_id)
            .collect();

        for message in messages {
            if let Some(reply_to) = &message.reply_to
                && !active_targets.contains(reply_to)
            {
                message.reply_to = None;
            }
            if let Some(thread_root) = &message.thread_root
                && !active_targets.contains(thread_root)
            {
                message.thread_root = None;
            }
        }
        Ok(())
    }

    /// Overlays the current joined member profile onto message authors so
    /// the public read path reflects live display-name and avatar changes
    /// (renames and MSC4466 profile propagation update `room_members`).
    ///
    /// The stored snapshot columns remain as the fallback: members who left
    /// the room (or whose member state was never seen) keep the value
    /// captured at projection time.
    async fn enrich_author_profiles(&self, messages: &mut [Message]) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }
        let room_ids: Vec<String> = messages
            .iter()
            .map(|message| message.room_id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let members = room_members::Entity::find()
            .filter(room_members::Column::RoomId.is_in(room_ids))
            .filter(room_members::Column::Membership.eq("join"))
            .all(&self.db)
            .await?;
        let by_key: HashMap<(String, String), &room_members::Model> = members
            .iter()
            .map(|member| ((member.room_id.clone(), member.user_id.clone()), member))
            .collect();
        for message in messages {
            if let Some(member) =
                by_key.get(&(message.room_id.clone(), message.sender_mxid.clone()))
            {
                message.author.display_name = member.display_name.clone();
                message.author.avatar_url = member.avatar_url.clone();
            }
        }
        Ok(())
    }

    /// Aggregated reaction summaries keyed by message event ID. Redacted
    /// reactions are excluded and multiple reactions from one sender collapse
    /// into a single count.
    ///
    /// Reactor sample (Phase 1): the same active unique-sender set is used to
    /// deterministically select up to 5 senders per (message,key). See
    /// `reaction_aggregate_map` and `misc/design/reaction-reactors.md` §3-5.
    async fn reaction_summary_map(
        &self,
        message_event_ids: &[String],
    ) -> Result<HashMap<String, Vec<ReactionSummary>>> {
        let aggregates = self.reaction_aggregate_map(message_event_ids).await?;
        Ok(aggregates
            .into_iter()
            .map(|(event_id, aggs)| {
                let mut summaries: Vec<ReactionSummary> = aggs
                    .into_iter()
                    .map(|agg| ReactionSummary {
                        key: agg.key,
                        count: agg.count,
                        mine: false,
                    })
                    .collect();
                summaries.sort_by(|a, b| a.key.cmp(&b.key));
                (event_id, summaries)
            })
            .collect())
    }

    /// Internal aggregate with bounded reactor sender sample.
    ///
    /// `count` and `selected_senders` derive from the **same** active unique
    /// sender set (single DB scan, no second query in the hydrate path).
    /// `selected_senders` is ordered by representative
    /// `origin_server_ts DESC, event_id DESC, sender_mxid ASC` and truncated
    /// to `REACTION_SAMPLE_LIMIT` (5). Internal `sender_mxid`/`event_id` are
    /// never exposed through the public `ReactionSummary` in Phase 1.
    pub async fn reaction_aggregate_map(
        &self,
        message_event_ids: &[String],
    ) -> Result<HashMap<String, Vec<ReactionAggregate>>> {
        if message_event_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut rows = reactions::Entity::find()
            .filter(reactions::Column::MessageEventId.is_in(message_event_ids.iter().cloned()))
            .filter(reactions::Column::RedactedAt.is_null())
            .all(&self.db)
            .await?;
        let active_parents = active_message_ids(
            &self.db,
            rows.iter()
                .map(|row| row.message_event_id.clone())
                .collect(),
        )
        .await?;
        rows.retain(|row| active_parents.contains(&row.message_event_id));
        Ok(Self::aggregate_reaction_rows(rows))
    }

    pub const REACTION_SAMPLE_LIMIT: usize = 5;

    fn aggregate_reaction_rows(
        rows: Vec<reactions::Model>,
    ) -> HashMap<String, Vec<ReactionAggregate>> {
        use std::collections::hash_map::Entry;
        // (message_event_id, key) -> sender -> rep (origin_server_ts, event_id)
        let mut per_key_sender_rep: HashMap<
            String,
            HashMap<String, HashMap<String, (i64, String)>>,
        > = HashMap::new();
        for row in rows {
            let key_map = per_key_sender_rep
                .entry(row.message_event_id)
                .or_default()
                .entry(row.key)
                .or_default();
            match key_map.entry(row.sender_mxid) {
                Entry::Vacant(v) => {
                    v.insert((row.origin_server_ts, row.event_id));
                }
                Entry::Occupied(mut o) => {
                    let cur = o.get();
                    if (row.origin_server_ts, &row.event_id) > (cur.0, &cur.1) {
                        o.insert((row.origin_server_ts, row.event_id));
                    }
                }
            }
        }
        let mut out: HashMap<String, Vec<ReactionAggregate>> = HashMap::new();
        for (msg_id, keys) in per_key_sender_rep {
            let mut aggregates = Vec::new();
            for (key, sender_rep) in keys {
                let count = sender_rep.len() as i64;
                // sort senders by rep ts DESC, event_id DESC, sender ASC
                let mut senders: Vec<(String, i64, String)> = sender_rep
                    .into_iter()
                    .map(|(sender, (ts, eid))| (sender, ts, eid))
                    .collect();
                senders.sort_by(|a, b| {
                    b.1.cmp(&a.1)
                        .then_with(|| b.2.cmp(&a.2))
                        .then_with(|| a.0.cmp(&b.0))
                });
                let selected_senders: Vec<String> = senders
                    .into_iter()
                    .take(Self::REACTION_SAMPLE_LIMIT)
                    .map(|(sender, _, _)| sender)
                    .collect();
                aggregates.push(ReactionAggregate {
                    key,
                    count,
                    selected_senders,
                });
            }
            aggregates.sort_by(|a, b| a.key.cmp(&b.key));
            out.insert(msg_id, aggregates);
        }
        out
    }

    /// Aggregated poll response summaries keyed by poll message ID. Redacted
    /// votes are excluded.
    async fn poll_response_summary_map(
        &self,
        poll_message_ids: &[String],
    ) -> Result<HashMap<String, Vec<PollResponseSummary>>> {
        if poll_message_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut rows = poll_response_events::Entity::find()
            .filter(
                poll_response_events::COLUMN
                    .poll_message_id
                    .is_in(poll_message_ids.iter().cloned()),
            )
            .filter(poll_response_events::Column::RedactedAt.is_null())
            .all(&self.db)
            .await?;
        let active_parents = active_message_ids(
            &self.db,
            rows.iter().map(|row| row.poll_message_id.clone()).collect(),
        )
        .await?;
        rows.retain(|row| active_parents.contains(&row.poll_message_id));

        let poll_rows = messages::Entity::find()
            .filter(
                messages::COLUMN.event_id.is_in(
                    rows.iter()
                        .map(|row| row.poll_message_id.clone())
                        .collect::<Vec<_>>(),
                ),
            )
            .all(&self.db)
            .await?;
        let answer_indexes: HashMap<String, Vec<String>> = poll_rows
            .into_iter()
            .filter_map(|row| {
                let content = serde_json::from_str::<Content>(&row.content_json).ok()?;
                let Content::Poll(poll) = content else {
                    return None;
                };
                Some((
                    row.event_id,
                    poll.options.into_iter().map(|option| option.id).collect(),
                ))
            })
            .collect();

        // Select each voter's latest non-redacted relation event. An event with
        // no mapped option is a spoiled/unvote response and contributes nothing.
        let mut latest_by_voter: HashMap<(String, String), poll_response_events::Model> =
            HashMap::new();
        for row in rows {
            let key = (row.poll_message_id.clone(), row.sender_mxid.clone());
            match latest_by_voter.get_mut(&key) {
                Some(current)
                    if (current.origin_server_ts, &current.event_id)
                        >= (row.origin_server_ts, &row.event_id) => {}
                _ => {
                    latest_by_voter.insert(key, row);
                }
            }
        }

        let mut counts_by_poll: HashMap<String, HashMap<i64, i64>> = HashMap::new();
        for (_, row) in latest_by_voter {
            let selections: Vec<String> =
                serde_json::from_str(&row.answer_ids_json).unwrap_or_default();
            let Some(options) = answer_indexes.get(&row.poll_message_id) else {
                continue;
            };
            if selections.is_empty() {
                if let Some(legacy_option_index) = row.option_index {
                    *counts_by_poll
                        .entry(row.poll_message_id)
                        .or_default()
                        .entry(legacy_option_index)
                        .or_default() += 1;
                }
            } else {
                let counts = counts_by_poll.entry(row.poll_message_id).or_default();
                for selection in selections {
                    if let Some(index) = options.iter().position(|option| option == &selection) {
                        *counts.entry(index as i64).or_default() += 1;
                    }
                }
            }
        }
        Ok(counts_by_poll
            .into_iter()
            .map(|(poll_id, counts)| {
                let mut summaries: Vec<PollResponseSummary> = counts
                    .into_iter()
                    .map(|(option_index, count)| PollResponseSummary {
                        option_index,
                        count,
                    })
                    .collect();
                summaries.sort_by_key(|s| s.option_index);
                (poll_id, summaries)
            })
            .collect())
    }
}

/// Annotation aggregates are part of a live comment's public view; suppress
/// them when the parent is redacted or has disappeared from the read model.
async fn active_message_ids(
    db: &sea_orm::DatabaseConnection,
    event_ids: Vec<String>,
) -> Result<std::collections::HashSet<String>> {
    if event_ids.is_empty() {
        return Ok(Default::default());
    }
    let models = messages::Entity::find()
        .filter(messages::COLUMN.event_id.is_in(event_ids))
        .filter(messages::COLUMN.status.eq(MessageStatus::Active.as_str()))
        .all(db)
        .await?;
    Ok(models.into_iter().map(|model| model.event_id).collect())
}
