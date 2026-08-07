use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

pub use sea_orm_migration::MigratorTrait;

pub mod m20260531_000001_initial_schema;
pub mod m20260619_000002_virtual_users;
pub mod m20260808_000003_comment_owner_hash;
pub mod m20260808_000004_intent_retry;
pub mod m20260808_000005_intent_room_id;

pub struct Migrator;

/// Whether a column already exists on a table (SQLite).
///
/// Entity-first migrations create tables from the *current* entity models, so
/// columns added by later migrations may already exist on fresh databases.
/// Column additions must therefore be idempotent.
pub(crate) async fn column_exists(
    manager: &SchemaManager<'_>,
    table: &str,
    column: &str,
) -> Result<bool, DbErr> {
    let db = manager.get_connection();
    // Table names come from internal constants, so direct interpolation is safe.
    let sql = format!("PRAGMA table_info({})", table);
    let rows = db
        .query_all_raw(Statement::from_string(manager.get_database_backend(), sql))
        .await?;
    for row in rows {
        if row
            .try_get::<String>("", "name")
            .map(|name| name == column)
            .unwrap_or(false)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260531_000001_initial_schema::Migration),
            Box::new(m20260619_000002_virtual_users::Migration),
            Box::new(m20260808_000003_comment_owner_hash::Migration),
            Box::new(m20260808_000004_intent_retry::Migration),
            Box::new(m20260808_000005_intent_room_id::Migration),
        ]
    }
}
