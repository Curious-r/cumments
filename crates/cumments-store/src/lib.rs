use anyhow::Result;
use async_trait::async_trait;
use cumments_core::identity::derive_visitor_id_from_public_key;
use cumments_core::intents::{DeleteCommentIntent, PostCommentIntent, UpdateCommentIntent};
use cumments_core::models::{Comment, PostSlug, Site, SiteId};
use cumments_core::ports::{
    BackfillCursorStore, CommentStore, IntentStore, RegistryStore, SiteStore, VirtualUserStore,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, EntityTrait, PaginatorTrait,
    QueryFilter, QueryOrder, Set, UpdateMany,
};
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use std::path::Path;

use crate::entities::active_enums::IntentStatus;

pub mod entities;
pub mod migration;

/// Maximum failed attempts before an intent is marked `failed` (dead-lettered).
const MAX_RETRIES: i64 = 5;
/// Base exponential-backoff delay after the first failure.
const BASE_BACKOFF_SECS: i64 = 30;
/// Upper bound for the backoff delay.
const MAX_BACKOFF_SECS: i64 = 1800;

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

/// A database-backed implementation of the storage ports.
#[derive(Clone)]
pub struct DbStore {
    db: DatabaseConnection,
}

impl DbStore {
    /// Creates a new DbStore by connecting to the database at the given URL.
    pub async fn connect(url: &str) -> Result<Self> {
        use migration::MigratorTrait;
        // SQLite (via sqlx) does not create missing database files by default
        // (create_if_missing=false); the README promises auto-creation, so
        // pre-create an empty file for plain sqlite paths.
        ensure_sqlite_file_exists(url)?;
        let db = sea_orm::Database::connect(url).await?;
        migration::Migrator::up(&db, None).await?;
        Ok(Self { db })
    }

    /// Create a consistent standalone SQLite snapshot at `destination`.
    ///
    /// First runs a WAL checkpoint so uncheckpointed writes are folded into the
    /// main database file, then uses `VACUUM INTO` to write a single-file copy.
    /// The destination must not already exist; the backup is a plain read-model
    /// snapshot and can always be replaced by `cumments backfill`.
    pub async fn backup_to(&self, destination: &Path) -> Result<()> {
        if destination.exists() {
            anyhow::bail!("destination already exists: {}", destination.display());
        }

        if let Some(parent) = destination.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        // Fold any WAL frames into the main database before snapshotting.
        self.db
            .execute_raw(Statement::from_string(
                DatabaseBackend::Sqlite,
                "PRAGMA wal_checkpoint(TRUNCATE)".to_owned(),
            ))
            .await?;

        // `VACUUM INTO` produces a compact, consistent copy that includes the
        // current logical contents regardless of journal mode.
        let sql = format!(
            "VACUUM INTO '{}'",
            destination.to_string_lossy().replace('\'', "''")
        );
        self.db
            .execute_raw(Statement::from_string(DatabaseBackend::Sqlite, sql))
            .await?;

        Ok(())
    }
}

/// Pre-create the SQLite database file when the URL points at a plain file
/// path that does not exist yet. In-memory databases are left untouched.
fn ensure_sqlite_file_exists(url: &str) -> Result<()> {
    if !url.starts_with("sqlite:") {
        return Ok(());
    }

    let rest = url
        .trim_start_matches("sqlite://")
        .trim_start_matches("sqlite:");
    let path = rest.split('?').next().unwrap_or(rest);
    if path.is_empty() || path == ":memory:" || path.starts_with("file:") {
        return Ok(());
    }

    let file = std::path::Path::new(path);
    if !file.exists() {
        std::fs::File::create(file)?;
    }
    Ok(())
}

#[cfg(test)]
mod connect_tests {
    use super::*;

    #[test]
    fn sqlite_file_is_precreated() {
        let dir = std::path::Path::new("/tmp");
        let db = dir.join(format!("cumments-connect-test-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);

        let url = format!("sqlite://{}", db.display());
        ensure_sqlite_file_exists(&url).expect("precreate");
        assert!(db.exists(), "database file should be created");

        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn memory_and_other_schemes_are_untouched() {
        ensure_sqlite_file_exists("sqlite::memory:").expect("memory ok");
        ensure_sqlite_file_exists("sqlite:///tmp/does-not-exist.db?mode=memory").expect("query ok");
        ensure_sqlite_file_exists("postgres://localhost/db").expect("non-sqlite ok");
    }
}

#[cfg(test)]
mod backup_tests {
    use super::*;

    #[tokio::test]
    async fn backup_creates_a_standalone_database() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir();
        let source = dir.join(format!("cumments-backup-src-{}.db", unique));
        let destination = dir.join(format!("cumments-backup-dst-{}.db", unique));

        let store = DbStore::connect(&format!("sqlite://{}", source.display()))
            .await
            .expect("connect source");
        store.backup_to(&destination).await.expect("create backup");

        assert!(destination.exists(), "backup file should exist");
        assert!(
            destination.metadata().unwrap().len() > 0,
            "backup should not be empty"
        );

        // The backup must be a valid Cumments database (migrations runnable).
        DbStore::connect(&format!("sqlite://{}", destination.display()))
            .await
            .expect("open backup as a Cumments database");

        let _ = std::fs::remove_file(&source);
        let _ = std::fs::remove_file(&destination);
    }
}

use crate::entities::*;

impl DbStore {
    /// Applies a status transition to an intent-queue row, stamping `updated_at`.
    ///
    /// `customize` scopes the update (filter) and can set extra columns, e.g. the
    /// Matrix event ID recorded when an intent transitions to `waiting_for_sync`.
    async fn transition_status<E>(
        &self,
        status: IntentStatus,
        status_column: E::Column,
        updated_at_column: E::Column,
        customize: impl FnOnce(sea_orm::UpdateMany<E>) -> sea_orm::UpdateMany<E>,
    ) -> Result<()>
    where
        E: EntityTrait,
    {
        customize(
            E::update_many()
                .col_expr(status_column, sea_orm::sea_query::Expr::value(status))
                .col_expr(
                    updated_at_column,
                    sea_orm::sea_query::Expr::value(chrono::Utc::now()),
                ),
        )
        .exec(&self.db)
        .await?;
        Ok(())
    }
}

#[async_trait]
impl IntentStore for DbStore {
    async fn save_post_intent(&self, intent: &PostCommentIntent) -> Result<()> {
        let payload = serde_json::to_string(intent)?;

        let active_model = intent_queue_post_comment::ActiveModel {
            payload: Set(payload),
            status: Set(IntentStatus::Pending),
            retry_count: Set(0),
            author_public_key: Set(Some(intent.author_public_key.clone())),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        };

        active_model.insert(&self.db).await?;

        Ok(())
    }

    async fn save_delete_intent(&self, intent: &DeleteCommentIntent) -> Result<()> {
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

        active_model.insert(&self.db).await?;

        Ok(())
    }

    async fn save_update_intent(&self, intent: &UpdateCommentIntent) -> Result<()> {
        let active_model = intent_queue_update_comment::ActiveModel {
            site_id: Set(intent.site_id.as_str().to_owned()),
            post_slug: Set(intent.post_slug.as_str().to_owned()),
            event_id: Set(intent.event_id.clone()),
            content: Set(intent.content.clone()),
            author_public_key: Set(Some(intent.author_public_key.clone())),
            author_signature: Set(Some(intent.author_signature.clone())),
            status: Set(IntentStatus::Pending),
            retry_count: Set(0),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        };

        active_model.insert(&self.db).await?;

        Ok(())
    }

    async fn get_pending_post_intents(&self) -> Result<Vec<(i64, PostCommentIntent)>> {
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
            .all(&self.db)
            .await?;

        let mut intents = Vec::new();
        for m in models {
            let intent: PostCommentIntent = serde_json::from_str(&m.payload)?;
            intents.push((m.id, intent));
        }
        Ok(intents)
    }

    async fn get_pending_delete_intents(&self) -> Result<Vec<(i64, DeleteCommentIntent)>> {
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
            .all(&self.db)
            .await?;

        let mut intents = Vec::new();
        for m in models {
            let intent: DeleteCommentIntent = serde_json::from_str(&m.payload)?;
            intents.push((m.id, intent));
        }
        Ok(intents)
    }

    async fn get_pending_update_intents(&self) -> Result<Vec<(i64, UpdateCommentIntent)>> {
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
            };
            intents.push((m.id, intent));
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
                query.filter(intent_queue_post_comment::COLUMN.id.eq(id))
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

        if model.retry_count >= MAX_RETRIES {
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
                .exec(&self.db)
                .await?;
            Ok(false)
        } else {
            let next_attempt = chrono::Utc::now() + backoff_after(model.retry_count);
            intent_queue_post_comment::Entity::update_many()
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
                .exec(&self.db)
                .await?;
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

        if model.retry_count >= MAX_RETRIES {
            intent_queue_delete_comment::Entity::update_many()
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
                .exec(&self.db)
                .await?;
            Ok(false)
        } else {
            let next_attempt = chrono::Utc::now() + backoff_after(model.retry_count);
            intent_queue_delete_comment::Entity::update_many()
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
                .exec(&self.db)
                .await?;
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

        if model.retry_count >= MAX_RETRIES {
            intent_queue_update_comment::Entity::update_many()
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
                .exec(&self.db)
                .await?;
            Ok(false)
        } else {
            let next_attempt = chrono::Utc::now() + backoff_after(model.retry_count);
            intent_queue_update_comment::Entity::update_many()
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
                .exec(&self.db)
                .await?;
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
    ) -> Result<Vec<(i64, String, Option<String>)>> {
        let models = intent_queue_post_comment::Entity::find()
            .filter(
                intent_queue_post_comment::COLUMN
                    .status
                    .eq(IntentStatus::WaitingForSync),
            )
            .filter(intent_queue_post_comment::Column::UpdatedAt.lte(cutoff))
            .all(&self.db)
            .await?;

        Ok(models
            .into_iter()
            .filter_map(|m| {
                m.matrix_event_id
                    .map(|event_id| (m.id, event_id, m.room_id))
            })
            .collect())
    }

    async fn get_stuck_delete_intent_ids(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<i64>> {
        let models = intent_queue_delete_comment::Entity::find()
            .filter(
                intent_queue_delete_comment::COLUMN
                    .status
                    .eq(IntentStatus::WaitingForSync),
            )
            .filter(intent_queue_delete_comment::Column::UpdatedAt.lte(cutoff))
            .all(&self.db)
            .await?;

        Ok(models.into_iter().map(|m| m.id).collect())
    }

    async fn get_stuck_update_intent_ids(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<i64>> {
        let models = intent_queue_update_comment::Entity::find()
            .filter(
                intent_queue_update_comment::COLUMN
                    .status
                    .eq(IntentStatus::WaitingForSync),
            )
            .filter(intent_queue_update_comment::Column::UpdatedAt.lte(cutoff))
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
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn mark_delete_intent_completed(&self, target_event_id: &str) -> Result<()> {
        self.transition_status(
            IntentStatus::Completed,
            intent_queue_delete_comment::Column::Status,
            intent_queue_delete_comment::Column::UpdatedAt,
            |query: UpdateMany<intent_queue_delete_comment::Entity>| {
                query.filter(
                    intent_queue_delete_comment::COLUMN
                        .target_event_id
                        .eq(target_event_id),
                )
            },
        )
        .await
    }

    async fn mark_update_intent_completed(&self, event_id: &str) -> Result<()> {
        self.transition_status(
            IntentStatus::Completed,
            intent_queue_update_comment::Column::Status,
            intent_queue_update_comment::Column::UpdatedAt,
            |query: UpdateMany<intent_queue_update_comment::Entity>| {
                query.filter(intent_queue_update_comment::COLUMN.event_id.eq(event_id))
            },
        )
        .await
    }
}

#[async_trait]
impl CommentStore for DbStore {
    async fn get_comment(&self, event_id: &str) -> Result<Option<Comment>> {
        let model = comments::Entity::find()
            .filter(comments::COLUMN.event_id.eq(event_id))
            .one(&self.db)
            .await?;

        Ok(model.map(Comment::from))
    }

    async fn get_comments(
        &self,
        site_id: &SiteId,
        post_slug: &PostSlug,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<Comment>, i64)> {
        let site_id_str = site_id.as_str();
        let post_slug_str = post_slug.as_str();

        let query = comments::Entity::find()
            .filter(comments::COLUMN.site_id.eq(site_id_str))
            .filter(comments::COLUMN.post_slug.eq(post_slug_str))
            .order_by_desc(comments::Column::Timestamp);

        let count = query.clone().count(&self.db).await?;
        if limit <= 0 {
            return Ok((Vec::new(), count as i64));
        }

        let models = query
            .paginate(&self.db, limit as u64)
            .fetch_page((offset / limit) as u64)
            .await?;

        let comments = models.into_iter().map(Comment::from).collect();

        Ok((comments, count as i64))
    }

    async fn save_comment(
        &self,
        comment: &Comment,
        room_id: &str,
        _site_id: &SiteId,
        _post_slug: &PostSlug,
    ) -> Result<()> {
        let active_model = comments::ActiveModel {
            event_id: Set(comment.event_id.clone()),
            room_id: Set(room_id.to_owned()),
            site_id: Set(comment.site_id.clone()),
            post_slug: Set(comment.post_slug.clone()),
            author_mxid: Set("".to_string()), // Default value if not provided
            author_nickname: Set(comment.author_nickname.clone()),
            author_public_key: Set(comment.author_public_key.clone()),
            content: Set(comment.content.clone()),
            timestamp: Set(comment.timestamp),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        };

        comments::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(comments::Column::EventId)
                    .update_columns([
                        comments::Column::AuthorNickname,
                        comments::Column::Content,
                        comments::Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;

        Ok(())
    }

    async fn update_comment_content(&self, event_id: &str, content: &str) -> Result<bool> {
        let result = comments::Entity::update_many()
            .col_expr(
                comments::Column::Content,
                sea_orm::sea_query::Expr::value(content),
            )
            .col_expr(
                comments::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(chrono::Utc::now()),
            )
            .filter(comments::COLUMN.event_id.eq(event_id))
            .exec(&self.db)
            .await?;

        Ok(result.rows_affected > 0)
    }

    async fn delete_comment(&self, event_id: &str) -> Result<bool> {
        let result = comments::Entity::delete_many()
            .filter(comments::COLUMN.event_id.eq(event_id))
            .exec(&self.db)
            .await?;

        Ok(result.rows_affected > 0)
    }

    async fn get_author_nickname(&self, event_id: &str) -> Result<Option<String>> {
        let model = comments::Entity::find()
            .filter(comments::COLUMN.event_id.eq(event_id))
            .one(&self.db)
            .await?;

        Ok(model.and_then(|m| m.author_nickname))
    }

    async fn get_comment_author_public_key(&self, event_id: &str) -> Result<Option<String>> {
        let model = comments::Entity::find()
            .filter(comments::COLUMN.event_id.eq(event_id))
            .one(&self.db)
            .await?;

        Ok(model.and_then(|m| m.author_public_key))
    }
}

#[async_trait]
impl RegistryStore for DbStore {
    async fn get_registered_room(
        &self,
        site_id: &SiteId,
        post_slug: &PostSlug,
    ) -> Result<Option<String>> {
        let room = room_registry::Entity::find()
            .filter(room_registry::COLUMN.site_id.eq(site_id.as_str()))
            .filter(room_registry::COLUMN.post_slug.eq(post_slug.as_str()))
            .filter(room_registry::COLUMN.is_active.eq(true))
            .one(&self.db)
            .await?;

        Ok(room.map(|r| r.room_id))
    }

    async fn is_room_active(&self, room_id: &str) -> Result<Option<bool>> {
        let room = room_registry::Entity::find_by_id(room_id.to_owned())
            .one(&self.db)
            .await?;

        Ok(room.map(|r| r.is_active))
    }

    async fn get_registered_room_identity(
        &self,
        room_id: &str,
    ) -> Result<Option<(String, String)>> {
        let room = room_registry::Entity::find_by_id(room_id.to_owned())
            .one(&self.db)
            .await?;

        Ok(room.map(|r| (r.site_id, r.post_slug)))
    }

    async fn register_room(
        &self,
        room_id: &str,
        site_id: &SiteId,
        post_slug: &PostSlug,
    ) -> Result<()> {
        let active_model = room_registry::ActiveModel {
            room_id: Set(room_id.to_owned()),
            site_id: Set(site_id.as_str().to_owned()),
            post_slug: Set(post_slug.as_str().to_owned()),
            is_active: Set(true),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
        };

        room_registry::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(room_registry::Column::RoomId)
                    .update_column(room_registry::Column::IsActive)
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;

        Ok(())
    }

    async fn invalidate_room_registry(&self, room_id: &str) -> Result<()> {
        room_registry::Entity::update_many()
            .col_expr(
                room_registry::Column::IsActive,
                sea_orm::sea_query::Expr::value(false),
            )
            .col_expr(
                room_registry::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(chrono::Utc::now()),
            )
            .filter(room_registry::COLUMN.room_id.eq(room_id))
            .exec(&self.db)
            .await?;
        Ok(())
    }
}

#[async_trait]
impl SiteStore for DbStore {
    async fn get_site(&self, id: &SiteId) -> Result<Option<Site>> {
        let model = sites::Entity::find_by_id(id.as_str().to_owned())
            .one(&self.db)
            .await?;

        Ok(model.map(Site::from))
    }

    async fn get_site_by_space_id(&self, space_id: &str) -> Result<Option<Site>> {
        let model = sites::Entity::find()
            .filter(sites::COLUMN.matrix_space_id.eq(space_id))
            .one(&self.db)
            .await?;

        Ok(model.map(Site::from))
    }

    async fn save_site(&self, site: &Site) -> Result<()> {
        let active_model = sites::ActiveModel {
            id: Set(site.id.clone()),
            matrix_space_id: Set(site.matrix_space_id.clone()),
            display_name: Set(site.display_name.clone()),
            created_at: Set(site.created_at),
        };

        sites::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(sites::Column::Id)
                    .update_columns([sites::Column::MatrixSpaceId, sites::Column::DisplayName])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;

        Ok(())
    }

    async fn ensure_site_exists(&self, site_id: &str, matrix_space_id: &str) -> Result<()> {
        let active_model = sites::ActiveModel {
            id: Set(site_id.to_owned()),
            matrix_space_id: Set(matrix_space_id.to_owned()),
            display_name: Set(Some(site_id.to_owned())),
            created_at: Set(chrono::Utc::now()),
        };

        sites::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(sites::Column::Id)
                    .do_nothing()
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;

        Ok(())
    }
}

impl From<comments::Model> for Comment {
    fn from(model: comments::Model) -> Self {
        Comment {
            event_id: model.event_id,
            site_id: model.site_id,
            post_slug: model.post_slug,
            author_nickname: model.author_nickname,
            author_public_key: model.author_public_key,
            content: model.content,
            timestamp: model.timestamp,
        }
    }
}

impl From<sites::Model> for Site {
    fn from(model: sites::Model) -> Self {
        Site {
            id: model.id,
            matrix_space_id: model.matrix_space_id,
            display_name: model.display_name,
            created_at: model.created_at,
        }
    }
}

#[async_trait]
impl VirtualUserStore for DbStore {
    async fn get_or_create_virtual_user(
        &self,
        author_public_key: &str,
        site_id: &SiteId,
        server_name: &str,
    ) -> Result<String> {
        // 1. Compute the deterministic virtual user ID
        let visitor_id = derive_visitor_id_from_public_key(author_public_key)
            .ok_or_else(|| anyhow::anyhow!("invalid author public key"))?;
        let virtual_user_id = format!(
            "@_cumments_{}_{}:{}",
            site_id.as_str(),
            visitor_id,
            server_name
        );

        // 2. Try to insert – on conflict (fingerprint + site_id already exists), do nothing
        let active_model = virtual_users::ActiveModel {
            fingerprint: Set(author_public_key.to_owned()),
            site_id: Set(site_id.as_str().to_owned()),
            virtual_user_id: Set(virtual_user_id.clone()),
            server_name: Set(server_name.to_owned()),
            created_at: Set(chrono::Utc::now()),
        };

        virtual_users::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::columns([
                    virtual_users::Column::Fingerprint,
                    virtual_users::Column::SiteId,
                ])
                .do_nothing()
                .to_owned(),
            )
            .exec(&self.db)
            .await?;

        Ok(virtual_user_id)
    }
}

#[async_trait]
impl BackfillCursorStore for DbStore {
    async fn get_cursor(&self, room_id: &str) -> Result<Option<String>> {
        let model = backfill_cursors::Entity::find_by_id(room_id.to_owned())
            .one(&self.db)
            .await?;
        Ok(model.and_then(|m| m.next_batch))
    }

    async fn save_cursor(&self, room_id: &str, next_batch: &str) -> Result<()> {
        let active_model = backfill_cursors::ActiveModel {
            room_id: Set(room_id.to_owned()),
            next_batch: Set(Some(next_batch.to_owned())),
            updated_at: Set(chrono::Utc::now()),
        };

        backfill_cursors::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(backfill_cursors::Column::RoomId)
                    .update_columns([
                        backfill_cursors::Column::NextBatch,
                        backfill_cursors::Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;
        Ok(())
    }
}
