use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

pub use sea_orm_migration::MigratorTrait;

pub mod m20260531_000001_initial_schema;
pub mod m20260619_000002_virtual_users;
pub mod m20260808_000003_comments_owner_hash;
pub mod m20260808_000004_intent_retry;
pub mod m20260808_000005_intent_room_id;
pub mod m20260808_000006_public_key_identity;
pub mod m20260808_000007_backfill_cursors_create;
pub mod m20260808_000008_author_challenge;
pub mod m20260808_000009_comments_reply_to;
pub mod m20260811_000010_comments_author_type;
pub mod m20260811_000011_virtual_users_public_key;
pub mod m20260811_000012_comments_sender_mxid;
pub mod m20260811_000013_comments_author_displayname;
pub mod m20260811_000014_backfill_cursors_next_token;
pub mod m20260811_000015_comments_author_display_name;
pub mod m20260812_000016_site_authentication;
pub mod m20260812_000017_comments_author_type_fix;
pub mod m20260812_000018_comments_edit_recency;
pub mod m20260812_000019_room_registry_unique_active;
pub mod m20260812_000020_backfill_tombstones;
pub mod m20260812_000021_post_intent_timeout_confirmations;
pub mod m20260812_000022_verification_token_attempts;
pub mod m20260812_000023_intent_queue_indexes;
pub mod m20260812_000024_post_intent_timeout_check_errors;
pub mod m20260812_000025_room_registry_blocked_reason;
pub mod m20260812_000026_post_intent_last_timeout_confirmation_at;
pub mod m20260812_000027_comments_intent_id;
pub mod m20260812_000028_idempotency_keys;

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
            Box::new(m20260808_000003_comments_owner_hash::Migration),
            Box::new(m20260808_000004_intent_retry::Migration),
            Box::new(m20260808_000005_intent_room_id::Migration),
            Box::new(m20260808_000006_public_key_identity::Migration),
            Box::new(m20260808_000007_backfill_cursors_create::Migration),
            Box::new(m20260808_000008_author_challenge::Migration),
            Box::new(m20260808_000009_comments_reply_to::Migration),
            Box::new(m20260811_000010_comments_author_type::Migration),
            Box::new(m20260811_000011_virtual_users_public_key::Migration),
            Box::new(m20260811_000012_comments_sender_mxid::Migration),
            Box::new(m20260811_000013_comments_author_displayname::Migration),
            Box::new(m20260811_000014_backfill_cursors_next_token::Migration),
            Box::new(m20260811_000015_comments_author_display_name::Migration),
            Box::new(m20260812_000016_site_authentication::Migration),
            Box::new(m20260812_000017_comments_author_type_fix::Migration),
            Box::new(m20260812_000018_comments_edit_recency::Migration),
            Box::new(m20260812_000019_room_registry_unique_active::Migration),
            Box::new(m20260812_000020_backfill_tombstones::Migration),
            Box::new(m20260812_000021_post_intent_timeout_confirmations::Migration),
            Box::new(m20260812_000022_verification_token_attempts::Migration),
            Box::new(m20260812_000023_intent_queue_indexes::Migration),
            Box::new(m20260812_000024_post_intent_timeout_check_errors::Migration),
            Box::new(m20260812_000025_room_registry_blocked_reason::Migration),
            Box::new(m20260812_000026_post_intent_last_timeout_confirmation_at::Migration),
            Box::new(m20260812_000027_comments_intent_id::Migration),
            Box::new(m20260812_000028_idempotency_keys::Migration),
        ]
    }
}
