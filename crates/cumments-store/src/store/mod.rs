use anyhow::Result;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use sea_orm::{DatabaseConnection, EntityTrait};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use crate::entities::active_enums::IntentStatus;

pub mod backfill;
pub mod identity;
pub mod intents;
pub mod messages;
pub mod registry;
pub mod site_auth;

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

        // The snapshot contains site secrets; keep it operator-readable only.
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o600))?;

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
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(file)?;
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
