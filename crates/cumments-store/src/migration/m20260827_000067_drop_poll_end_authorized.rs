use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

const TABLE: &str = "poll_end_events";

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Drop the projection-time `authorized` snapshot from `poll_end_events`.
///
/// End authorization depends on the room state at the event's position in the
/// Matrix DAG, not on the power levels visible when the event happened to be
/// processed locally, so it must never be persisted as a Poll fact. The column
/// is derived at read time from the canonical facts instead. Fresh databases
/// never create it (the entity no longer declares it); this migration removes
/// it from databases that already applied the earlier schema.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if column_exists(manager, TABLE, "authorized").await? {
            manager
                .get_connection()
                .execute_unprepared(&format!("ALTER TABLE {TABLE} DROP COLUMN authorized"))
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // The snapshot was intentionally removed; it is not reconstructible.
        if !column_exists(manager, TABLE, "authorized").await? {
            manager
                .get_connection()
                .execute_unprepared(&format!(
                    "ALTER TABLE {TABLE} ADD COLUMN authorized BOOLEAN NOT NULL DEFAULT 0"
                ))
                .await?;
        }
        Ok(())
    }
}
