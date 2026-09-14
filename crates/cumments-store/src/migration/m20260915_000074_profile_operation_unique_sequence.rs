use sea_orm::Statement;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Enforces uniqueness on (site_id, author_public_key, field, sequence)
/// in profile_operations to guarantee race-free serialization streams.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // 1. Validate that no duplicate sequences exist in any serialization stream.
        // We must NEVER silently delete durable profile operations.
        let check_sql = "SELECT site_id, author_public_key, field, sequence, COUNT(*) as count \
                         FROM profile_operations \
                         GROUP BY site_id, author_public_key, field, sequence \
                         HAVING count > 1 \
                         LIMIT 1";

        let duplicate = db
            .query_one_raw(Statement::from_string(
                manager.get_database_backend(),
                check_sql.to_string(),
            ))
            .await?;

        if let Some(row) = duplicate {
            let site_id: String = row.try_get("", "site_id")?;
            let author_public_key: String = row.try_get("", "author_public_key")?;
            let field: String = row.try_get("", "field")?;
            let sequence: i64 = row.try_get("", "sequence")?;
            let count: i64 = row.try_get("", "count")?;

            return Err(DbErr::Custom(format!(
                "Cannot apply unique sequence migration: duplicate profile operation sequence detected for \
                 site_id='{site_id}', author_public_key='{author_public_key}', field='{field}', sequence={sequence} \
                 (found {count} operations with the same sequence). Manual resolution required to preserve operation history."
            )));
        }

        // 2. Drop the non-unique index from migration 073 and create the unique index.
        db.execute_unprepared(
            "DROP INDEX IF EXISTS idx_profile_ops_site_author_field; \
             CREATE UNIQUE INDEX IF NOT EXISTS uq_profile_ops_site_author_field \
             ON profile_operations (site_id, author_public_key, field, sequence);",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DROP INDEX IF EXISTS uq_profile_ops_site_author_field; \
                 DROP INDEX IF EXISTS idx_profile_ops_site_author_field; \
                 CREATE INDEX IF NOT EXISTS idx_profile_ops_site_author_field \
                 ON profile_operations (site_id, author_public_key, field, sequence);",
            )
            .await?;
        Ok(())
    }
}
