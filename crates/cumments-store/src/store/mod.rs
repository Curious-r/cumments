use anyhow::Result;
use async_trait::async_trait;
use cumments_core::identity::derive_guest_id_from_public_key;
use cumments_core::models::SiteId;
use cumments_core::ports::{BackfillCursorStore, VirtualUserStore};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use std::path::Path;

use crate::entities::active_enums::IntentStatus;

pub mod comments;
pub mod intents;
pub mod registry;

/// A database-backed implementation of the storage ports.
#[derive(Clone)]
pub struct DbStore {
    db: DatabaseConnection,
}

impl DbStore {
    /// Creates a new DbStore by connecting to the database at the given URL.
    pub async fn connect(url: &str) -> Result<Self> {
        use crate::migration::MigratorTrait;
        // SQLite (via sqlx) does not create missing database files by default
        // (create_if_missing=false); the README promises auto-creation, so
        // pre-create an empty file for plain sqlite paths.
        ensure_sqlite_file_exists(url)?;
        let db = sea_orm::Database::connect(url).await?;
        crate::migration::Migrator::up(&db, None).await?;
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
impl VirtualUserStore for DbStore {
    async fn get_or_create_virtual_user(
        &self,
        author_public_key: &str,
        site_id: &SiteId,
        server_name: &str,
    ) -> Result<String> {
        // 1. Compute the deterministic virtual user ID
        let guest_id = derive_guest_id_from_public_key(author_public_key)
            .ok_or_else(|| anyhow::anyhow!("invalid author public key"))?;
        let virtual_user_id = format!(
            "@_cumments_{}_{}:{}",
            site_id.as_str(),
            guest_id,
            server_name
        );

        // The mapping is stable per (public key, site): return the stored
        // virtual user even when the current server_name differs (e.g. after
        // a domain migration), so edits keep matching the original sender.
        if let Some(existing) = virtual_users::Entity::find()
            .filter(virtual_users::Column::PublicKey.eq(author_public_key))
            .filter(virtual_users::Column::SiteId.eq(site_id.as_str()))
            .one(&self.db)
            .await?
        {
            return Ok(existing.virtual_user_id);
        }

        // 2. Try to insert – on conflict (public_key + site_id already exists), do nothing
        let active_model = virtual_users::ActiveModel {
            public_key: Set(author_public_key.to_owned()),
            site_id: Set(site_id.as_str().to_owned()),
            virtual_user_id: Set(virtual_user_id.clone()),
            server_name: Set(server_name.to_owned()),
            created_at: Set(chrono::Utc::now()),
        };

        virtual_users::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::columns([
                    virtual_users::Column::PublicKey,
                    virtual_users::Column::SiteId,
                ])
                .do_nothing()
                .to_owned(),
            )
            .exec(&self.db)
            .await?;

        // Re-read after the insert: a concurrent request may have won the
        // race, and the winner's stored ID is authoritative.
        if let Some(existing) = virtual_users::Entity::find()
            .filter(virtual_users::Column::PublicKey.eq(author_public_key))
            .filter(virtual_users::Column::SiteId.eq(site_id.as_str()))
            .one(&self.db)
            .await?
        {
            return Ok(existing.virtual_user_id);
        }

        Ok(virtual_user_id)
    }
}

#[async_trait]
impl BackfillCursorStore for DbStore {
    async fn get_cursor(&self, room_id: &str) -> Result<Option<String>> {
        let model = backfill_cursors::Entity::find_by_id(room_id.to_owned())
            .one(&self.db)
            .await?;
        Ok(model.and_then(|m| m.next_token))
    }

    async fn save_cursor(&self, room_id: &str, next_token: &str) -> Result<()> {
        let active_model = backfill_cursors::ActiveModel {
            room_id: Set(room_id.to_owned()),
            next_token: Set(Some(next_token.to_owned())),
            updated_at: Set(chrono::Utc::now()),
        };

        backfill_cursors::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(backfill_cursors::Column::RoomId)
                    .update_columns([
                        backfill_cursors::Column::NextToken,
                        backfill_cursors::Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;
        Ok(())
    }
}
