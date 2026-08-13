use super::DbStore;
use crate::entities::{
    backfill_tombstones, media_uploads, message_revisions, messages, poll_responses, reactions,
};
use anyhow::Result;
use async_trait::async_trait;
use cumments_core::models::{
    AuthorKind, AuthorSnapshot, Content, Message, MessagePage, MessageRevision, MessageStatus,
    PollResponseSummary, PollVote, PostSlug, Reaction, ReactionSummary, SiteId, UnknownContent,
};
use cumments_core::ports::MessageStore;
use sea_orm::{
    ColumnTrait, Condition, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set,
    TransactionTrait,
};
use std::collections::{HashMap, HashSet};

fn content_to_json(content: &Content) -> String {
    serde_json::to_string(content).expect("content serializes")
}

fn content_from_json(raw: &str) -> Content {
    serde_json::from_str(raw).unwrap_or(Content::Unknown(UnknownContent {
        fallback: None,
        raw: serde_json::Value::Null,
    }))
}

fn message_from_model(model: messages::Model) -> Message {
    let kind = if model.author_kind == "matrix" {
        AuthorKind::Matrix
    } else {
        AuthorKind::Guest
    };
    Message {
        event_id: model.event_id,
        site_id: model.site_id,
        post_slug: model.post_slug,
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
        content: content_from_json(&model.content_json),
        timestamp: model.timestamp,
        edited_at: model
            .last_edit_ts
            .and_then(chrono::DateTime::from_timestamp_millis),
        reply_to: model.reply_to,
        thread_root: model.thread_root,
        intent_id: model.intent_id,
        status: model.status.parse().unwrap_or(MessageStatus::Active),
        redacted_at: model.redacted_at,
        redacted_by: model.redacted_by,
        reactions: Vec::new(),
        room_id: model.room_id,
        sender_mxid: model.sender_mxid,
        raw_content: serde_json::from_str(&model.raw_content_json)
            .unwrap_or(serde_json::Value::Null),
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
        post_slug: &PostSlug,
        limit: i64,
        offset: i64,
    ) -> Result<MessagePage> {
        let site_id_str = site_id.as_str();
        let post_slug_str = post_slug.as_str();

        let query = messages::Entity::find()
            .filter(messages::COLUMN.site_id.eq(site_id_str))
            .filter(messages::COLUMN.post_slug.eq(post_slug_str))
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

        let mut items = Vec::with_capacity(models.len());
        for model in models {
            items.push(self.hydrate(message_from_model(model)).await?);
        }
        Ok(MessagePage {
            items,
            total: total as i64,
        })
    }

    async fn save_message(&self, message: &Message) -> Result<()> {
        let now = chrono::Utc::now();
        let active_model = messages::ActiveModel {
            event_id: Set(message.event_id.clone()),
            room_id: Set(message.room_id.clone()),
            site_id: Set(message.site_id.clone()),
            post_slug: Set(message.post_slug.clone()),
            sender_mxid: Set(message.sender_mxid.clone()),
            author_kind: Set(message.author.kind.as_str().to_string()),
            author_display_name: Set(message.author.display_name.clone()),
            author_avatar_url: Set(message.author.avatar_url.clone()),
            author_public_key: Set(message.author.public_key.clone()),
            content_json: Set(content_to_json(&message.content)),
            raw_content_json: Set(
                serde_json::to_string(&message.raw_content).unwrap_or_else(|_| "null".to_string())
            ),
            timestamp: Set(message.timestamp),
            reply_to: Set(message.reply_to.clone()),
            thread_root: Set(message.thread_root.clone()),
            status: Set(message.status.as_str().to_string()),
            redacted_at: Set(message.redacted_at),
            redacted_by: Set(message.redacted_by.clone()),
            intent_id: Set(message.intent_id),
            last_edit_ts: Set(message.edited_at.map(|t| t.timestamp_millis())),
            last_edit_event_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };

        messages::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(messages::Column::EventId)
                    .update_columns([
                        messages::Column::RoomId,
                        messages::Column::SiteId,
                        messages::Column::PostSlug,
                        messages::Column::SenderMxid,
                        messages::Column::AuthorKind,
                        messages::Column::AuthorDisplayName,
                        messages::Column::AuthorAvatarUrl,
                        messages::Column::AuthorPublicKey,
                        messages::Column::ContentJson,
                        messages::Column::RawContentJson,
                        messages::Column::Timestamp,
                        messages::Column::ReplyTo,
                        messages::Column::ThreadRoot,
                        messages::Column::IntentId,
                        messages::Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn apply_edit(&self, message: &Message, revision: &MessageRevision) -> Result<bool> {
        let txn = self.db.begin().await?;
        let edit_ts = revision.edited_at.timestamp_millis();

        // Only apply when the edit is newer than the last applied edit
        // (missing last_edit means the original is current), with the edit
        // event ID as the deterministic tie-breaker.
        let recency = Condition::any()
            .add(messages::Column::LastEditTs.is_null())
            .add(messages::Column::LastEditTs.lt(edit_ts))
            .add(
                Condition::all()
                    .add(messages::Column::LastEditTs.eq(edit_ts))
                    .add(messages::Column::LastEditEventId.lt(revision.event_id.clone())),
            );

        let result = messages::Entity::update_many()
            .col_expr(
                messages::Column::ContentJson,
                sea_orm::sea_query::Expr::value(content_to_json(&message.content)),
            )
            .col_expr(
                messages::Column::LastEditTs,
                sea_orm::sea_query::Expr::value(edit_ts),
            )
            .col_expr(
                messages::Column::LastEditEventId,
                sea_orm::sea_query::Expr::value(revision.event_id.clone()),
            )
            .col_expr(
                messages::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(chrono::Utc::now()),
            )
            .filter(messages::COLUMN.event_id.eq(message.event_id.clone()))
            .filter(messages::COLUMN.room_id.eq(message.room_id.clone()))
            .filter(recency)
            .exec(&txn)
            .await?;

        if result.rows_affected == 0 {
            txn.rollback().await?;
            return Ok(false);
        }

        let revision_model = message_revisions::ActiveModel {
            event_id: Set(revision.event_id.clone()),
            message_event_id: Set(message.event_id.clone()),
            content_json: Set(content_to_json(&revision.content)),
            edited_at: Set(revision.edited_at),
            editor_mxid: Set(revision.editor_mxid.clone()),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        };
        message_revisions::Entity::insert(revision_model)
            .exec(&txn)
            .await?;
        txn.commit().await?;
        Ok(true)
    }

    async fn redact_message(
        &self,
        event_id: &str,
        room_id: &str,
        redacted_at: chrono::DateTime<chrono::Utc>,
        redacted_by: &str,
    ) -> Result<bool> {
        let result = messages::Entity::update_many()
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
                messages::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(chrono::Utc::now()),
            )
            .filter(messages::COLUMN.event_id.eq(event_id))
            .filter(messages::COLUMN.room_id.eq(room_id))
            .exec(&self.db)
            .await?;
        Ok(result.rows_affected > 0)
    }

    async fn get_author_display_name(&self, event_id: &str) -> Result<Option<Option<String>>> {
        let model = messages::Entity::find()
            .filter(messages::COLUMN.event_id.eq(event_id))
            .one(&self.db)
            .await?;
        Ok(model.map(|m| m.author_display_name))
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

    async fn save_poll_vote(&self, vote: &PollVote) -> Result<()> {
        let txn = self.db.begin().await?;
        let existing = poll_responses::Entity::find()
            .filter(poll_responses::Column::PollMessageId.eq(&vote.poll_message_id))
            .filter(poll_responses::Column::SenderMxid.eq(&vote.sender_mxid))
            .one(&txn)
            .await?;
        let now = chrono::Utc::now();

        if let Some(existing) = existing {
            // Re-delivering the same vote event (push retry / backfill) must
            // not resurrect a redacted vote; any other event supersedes it
            // and clears the redaction state.
            if existing.event_id.as_deref() == Some(vote.event_id.as_str())
                && existing.redacted_at.is_some()
            {
                txn.commit().await?;
                return Ok(());
            }
            poll_responses::Entity::update_many()
                .col_expr(
                    poll_responses::Column::EventId,
                    sea_orm::sea_query::Expr::value(Some(vote.event_id.clone())),
                )
                .col_expr(
                    poll_responses::Column::OptionIndex,
                    sea_orm::sea_query::Expr::value(vote.option_index),
                )
                .col_expr(
                    poll_responses::Column::OriginServerTs,
                    sea_orm::sea_query::Expr::value(vote.origin_server_ts),
                )
                .col_expr(
                    poll_responses::Column::RedactedAt,
                    sea_orm::sea_query::Expr::value(None::<chrono::DateTime<chrono::Utc>>),
                )
                .col_expr(
                    poll_responses::Column::RedactedBy,
                    sea_orm::sea_query::Expr::value(None::<String>),
                )
                .col_expr(
                    poll_responses::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(now),
                )
                .filter(poll_responses::Column::PollMessageId.eq(&vote.poll_message_id))
                .filter(poll_responses::Column::SenderMxid.eq(&vote.sender_mxid))
                .exec(&txn)
                .await?;
            txn.commit().await?;
            return Ok(());
        }

        let active_model = poll_responses::ActiveModel {
            event_id: Set(Some(vote.event_id.clone())),
            poll_message_id: Set(vote.poll_message_id.clone()),
            sender_mxid: Set(vote.sender_mxid.clone()),
            option_index: Set(vote.option_index),
            origin_server_ts: Set(vote.origin_server_ts),
            redacted_at: Set(None),
            redacted_by: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        poll_responses::Entity::insert(active_model)
            .exec(&txn)
            .await?;
        txn.commit().await?;
        Ok(())
    }

    async fn get_poll_vote_by_event(&self, event_id: &str) -> Result<Option<PollVote>> {
        let model = poll_responses::Entity::find()
            .filter(poll_responses::Column::EventId.eq(event_id))
            .one(&self.db)
            .await?;
        Ok(model.map(|m| PollVote {
            event_id: m.event_id.unwrap_or_default(),
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
        let result = poll_responses::Entity::update_many()
            .col_expr(
                poll_responses::Column::RedactedAt,
                sea_orm::sea_query::Expr::value(Some(redacted_at)),
            )
            .col_expr(
                poll_responses::Column::RedactedBy,
                sea_orm::sea_query::Expr::value(Some(redacted_by.to_owned())),
            )
            .col_expr(
                poll_responses::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(chrono::Utc::now()),
            )
            .filter(poll_responses::Column::EventId.eq(event_id))
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
        post_slug: &str,
    ) -> Result<()> {
        let now = chrono::Utc::now();
        let model = media_uploads::ActiveModel {
            mxc_url: Set(mxc_url.to_owned()),
            author_public_key: Set(author_public_key.to_owned()),
            site_id: Set(site_id.to_owned()),
            post_slug: Set(post_slug.to_owned()),
            used_at: Set(None),
            created_at: Set(now),
            ..Default::default()
        };
        media_uploads::Entity::insert(model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(media_uploads::Column::MxcUrl)
                    .update_columns([
                        media_uploads::Column::AuthorPublicKey,
                        media_uploads::Column::SiteId,
                        media_uploads::Column::PostSlug,
                        media_uploads::Column::UsedAt,
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
        post_slug: &str,
    ) -> Result<bool> {
        let found = media_uploads::Entity::find()
            .filter(media_uploads::Column::MxcUrl.eq(mxc_url))
            .filter(media_uploads::Column::AuthorPublicKey.eq(author_public_key))
            .filter(media_uploads::Column::SiteId.eq(site_id))
            .filter(media_uploads::Column::PostSlug.eq(post_slug))
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
        let rows = media_uploads::Entity::find()
            .filter(media_uploads::Column::UsedAt.is_null())
            .filter(media_uploads::Column::CreatedAt.lt(cutoff))
            .all(&self.db)
            .await?;
        Ok(rows.into_iter().map(|row| row.mxc_url).collect())
    }

    async fn delete_media_upload(&self, mxc_url: &str) -> Result<()> {
        media_uploads::Entity::delete_many()
            .filter(media_uploads::Column::MxcUrl.eq(mxc_url))
            .exec(&self.db)
            .await?;
        Ok(())
    }
}

impl DbStore {
    /// Hydrate a message with its aggregated reactions and poll responses.
    async fn hydrate(&self, mut message: Message) -> Result<Message> {
        message.reactions = self.reaction_summaries(&message.event_id).await?;
        if let Content::Poll(poll) = &mut message.content {
            poll.responses = self.poll_response_summaries(&message.event_id).await?;
        }
        Ok(message)
    }

    async fn reaction_summaries(&self, message_event_id: &str) -> Result<Vec<ReactionSummary>> {
        let rows = reactions::Entity::find()
            .filter(reactions::Column::MessageEventId.eq(message_event_id))
            .filter(reactions::Column::RedactedAt.is_null())
            .all(&self.db)
            .await?;

        let mut senders_by_key: HashMap<String, HashSet<String>> = HashMap::new();
        for row in rows {
            senders_by_key
                .entry(row.key)
                .or_default()
                .insert(row.sender_mxid);
        }
        let mut summaries: Vec<_> = senders_by_key
            .into_iter()
            .map(|(key, senders)| ReactionSummary {
                key,
                count: senders.len() as i64,
            })
            .collect();
        summaries.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(summaries)
    }

    async fn poll_response_summaries(
        &self,
        poll_message_id: &str,
    ) -> Result<Vec<PollResponseSummary>> {
        let rows = poll_responses::Entity::find()
            .filter(poll_responses::Column::PollMessageId.eq(poll_message_id))
            .filter(poll_responses::Column::RedactedAt.is_null())
            .all(&self.db)
            .await?;

        let mut counts: HashMap<i64, i64> = HashMap::new();
        for row in rows {
            *counts.entry(row.option_index).or_default() += 1;
        }
        let mut summaries: Vec<_> = counts
            .into_iter()
            .map(|(option_index, count)| PollResponseSummary {
                option_index,
                count,
            })
            .collect();
        summaries.sort_by_key(|s| s.option_index);
        Ok(summaries)
    }
}
