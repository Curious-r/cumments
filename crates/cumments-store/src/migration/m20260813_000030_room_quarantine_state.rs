use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

const TABLE: &str = "room_registry";
const UNIQUE_INDEX: &str = "idx_room_registry_active_site_post";

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Replace the `is_active` + `blocked_reason` two-field encoding with an
/// explicit `status` lifecycle (`active`/`quarantined`/`superseded`) plus
/// quarantine metadata (reason, first quarantine time, failure count and the
/// next scheduled adoption attempt).
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(&format!("DROP INDEX IF EXISTS {UNIQUE_INDEX}"))
            .await?;

        if !column_exists(manager, TABLE, "status").await? {
            db.execute_unprepared(&format!(
                "ALTER TABLE {TABLE} ADD COLUMN status TEXT NOT NULL DEFAULT 'superseded'"
            ))
            .await?;
        }
        // Backfill from the legacy encoding when upgrading an existing DB.
        if column_exists(manager, TABLE, "is_active").await?
            && column_exists(manager, TABLE, "blocked_reason").await?
        {
            db.execute_unprepared(&format!(
                "UPDATE {TABLE} SET status = 'active' \
                 WHERE is_active = 1 AND blocked_reason IS NULL"
            ))
            .await?;
            db.execute_unprepared(&format!(
                "UPDATE {TABLE} SET status = 'quarantined' \
                 WHERE blocked_reason IS NOT NULL"
            ))
            .await?;
        }

        if column_exists(manager, TABLE, "blocked_reason").await?
            && !column_exists(manager, TABLE, "quarantine_reason").await?
        {
            db.execute_unprepared(&format!(
                "ALTER TABLE {TABLE} RENAME COLUMN blocked_reason TO quarantine_reason"
            ))
            .await?;
        }

        if !column_exists(manager, TABLE, "quarantined_at").await? {
            db.execute_unprepared(&format!(
                "ALTER TABLE {TABLE} ADD COLUMN quarantined_at DATETIME"
            ))
            .await?;
        }
        db.execute_unprepared(&format!(
            "UPDATE {TABLE} SET quarantined_at = updated_at \
             WHERE status = 'quarantined' AND quarantined_at IS NULL"
        ))
        .await?;

        if !column_exists(manager, TABLE, "adoption_failures").await? {
            db.execute_unprepared(&format!(
                "ALTER TABLE {TABLE} ADD COLUMN adoption_failures INTEGER NOT NULL DEFAULT 0"
            ))
            .await?;
        }
        db.execute_unprepared(&format!(
            "UPDATE {TABLE} SET adoption_failures = 1 \
             WHERE status = 'quarantined' AND adoption_failures = 0"
        ))
        .await?;

        if !column_exists(manager, TABLE, "next_attempt_at").await? {
            db.execute_unprepared(&format!(
                "ALTER TABLE {TABLE} ADD COLUMN next_attempt_at DATETIME"
            ))
            .await?;
        }

        if column_exists(manager, TABLE, "is_active").await? {
            db.execute_unprepared(&format!("ALTER TABLE {TABLE} DROP COLUMN is_active"))
                .await?;
        }

        db.execute_unprepared(&format!(
            "CREATE UNIQUE INDEX IF NOT EXISTS {UNIQUE_INDEX} \
             ON {TABLE}(site_id, post_slug) WHERE status = 'active'"
        ))
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(&format!("DROP INDEX IF EXISTS {UNIQUE_INDEX}"))
            .await?;

        if !column_exists(manager, TABLE, "is_active").await? {
            db.execute_unprepared(&format!(
                "ALTER TABLE {TABLE} ADD COLUMN is_active BOOLEAN NOT NULL DEFAULT 0"
            ))
            .await?;
        }
        db.execute_unprepared(&format!(
            "UPDATE {TABLE} SET is_active = 1 WHERE status = 'active'"
        ))
        .await?;

        if column_exists(manager, TABLE, "quarantine_reason").await?
            && !column_exists(manager, TABLE, "blocked_reason").await?
        {
            db.execute_unprepared(&format!(
                "ALTER TABLE {TABLE} RENAME COLUMN quarantine_reason TO blocked_reason"
            ))
            .await?;
        }

        for column in [
            "status",
            "quarantined_at",
            "adoption_failures",
            "next_attempt_at",
        ] {
            if column_exists(manager, TABLE, column).await? {
                db.execute_unprepared(&format!("ALTER TABLE {TABLE} DROP COLUMN {column}"))
                    .await?;
            }
        }

        db.execute_unprepared(&format!(
            "CREATE UNIQUE INDEX IF NOT EXISTS {UNIQUE_INDEX} \
             ON {TABLE}(site_id, post_slug) WHERE is_active = 1"
        ))
        .await?;
        Ok(())
    }
}
