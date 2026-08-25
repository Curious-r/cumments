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
pub mod m20260812_000029_comments_projected_at;
pub mod m20260813_000030_room_quarantine_state;
pub mod m20260813_000031_message_read_model;
pub mod m20260813_000032_room_metadata;
pub mod m20260813_000033_drop_room_registry_blocked_reason;
pub mod m20260813_000034_poll_response_redaction;
pub mod m20260813_000035_media_uploads;
pub mod m20260813_000036_poll_votes_unique_sender;
pub mod m20260813_000037_site_governance_roles;
pub mod m20260813_000038_role_claims;
pub mod m20260814_000039_sites_custom_id;
pub mod m20260814_000040_sites_lifecycle;
pub mod m20260814_000041_submissions_rename;
pub mod m20260814_000042_submission_leases;
pub mod m20260814_000043_media_upload_idempotency;
pub mod m20260814_000044_post_submission_fresh_txn;
pub mod m20260815_000045_post_submission_txn_id;
pub mod m20260815_000046_unified_submission_txn_ids;
pub mod m20260815_000047_role_claim_dm_room;
pub mod m20260815_000048_media_upload_submission;
pub mod m20260815_000049_command_audit_log;
pub mod m20260816_000050_sticker_packs;
pub mod m20260816_000051_media_uploads_post_slug_nullable;
pub mod m20260817_000052_terminology_rename;
pub mod m20260817_000053_site_transfers;
pub mod m20260823_000054_redacted_content;
pub mod m20260824_000055_edit_revision_facts;
pub mod m20260824_000056_poll_response_events;
pub mod m20260825_000057_appservice_txn_dedupe;
pub mod m20260825_000058_room_state_snapshots;
pub mod m20260825_000059_room_upgrade_intents;
pub mod m20260825_000060_sanitize_redacted_payloads;
pub mod m20260825_000061_clear_redacted_poll_choices;
pub mod m20260826_000062_projection_repairs;
pub mod m20260826_000063_poll_answer_selections;

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

/// The slug column name in `table`: `page_slug` on fresh entity-first
/// databases, `post_slug` on databases created before the terminology
/// rename. Historical migrations use it so their SQL matches whichever
/// schema is present.
pub(crate) async fn slug_column(manager: &SchemaManager<'_>, table: &str) -> Result<String, DbErr> {
    if column_exists(manager, table, "page_slug").await? {
        Ok("page_slug".to_string())
    } else {
        Ok("post_slug".to_string())
    }
}

/// Whether a table exists in the current SQLite database.
pub(crate) async fn table_exists(manager: &SchemaManager<'_>, table: &str) -> Result<bool, DbErr> {
    let db = manager.get_connection();
    // Table names come from internal constants, so direct interpolation is safe.
    let sql =
        format!("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '{table}'");
    let rows = db
        .query_all_raw(Statement::from_string(manager.get_database_backend(), sql))
        .await?;
    Ok(rows
        .first()
        .and_then(|row| row.try_get::<i64>("", "COUNT(*)").ok())
        .unwrap_or(0)
        > 0)
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
            Box::new(m20260812_000029_comments_projected_at::Migration),
            Box::new(m20260813_000030_room_quarantine_state::Migration),
            Box::new(m20260813_000031_message_read_model::Migration),
            Box::new(m20260813_000032_room_metadata::Migration),
            Box::new(m20260813_000033_drop_room_registry_blocked_reason::Migration),
            Box::new(m20260813_000034_poll_response_redaction::Migration),
            Box::new(m20260813_000035_media_uploads::Migration),
            Box::new(m20260813_000036_poll_votes_unique_sender::Migration),
            Box::new(m20260813_000037_site_governance_roles::Migration),
            Box::new(m20260813_000038_role_claims::Migration),
            Box::new(m20260814_000039_sites_custom_id::Migration),
            Box::new(m20260814_000040_sites_lifecycle::Migration),
            Box::new(m20260814_000041_submissions_rename::Migration),
            Box::new(m20260814_000042_submission_leases::Migration),
            Box::new(m20260814_000043_media_upload_idempotency::Migration),
            Box::new(m20260814_000044_post_submission_fresh_txn::Migration),
            Box::new(m20260815_000045_post_submission_txn_id::Migration),
            Box::new(m20260815_000046_unified_submission_txn_ids::Migration),
            Box::new(m20260815_000047_role_claim_dm_room::Migration),
            Box::new(m20260815_000048_media_upload_submission::Migration),
            Box::new(m20260815_000049_command_audit_log::Migration),
            Box::new(m20260816_000050_sticker_packs::Migration),
            Box::new(m20260816_000051_media_uploads_post_slug_nullable::Migration),
            Box::new(m20260817_000052_terminology_rename::Migration),
            Box::new(m20260817_000053_site_transfers::Migration),
            Box::new(m20260823_000054_redacted_content::Migration),
            Box::new(m20260824_000055_edit_revision_facts::Migration),
            Box::new(m20260824_000056_poll_response_events::Migration),
            Box::new(m20260825_000057_appservice_txn_dedupe::Migration),
            Box::new(m20260825_000058_room_state_snapshots::Migration),
            Box::new(m20260825_000059_room_upgrade_intents::Migration),
            Box::new(m20260825_000060_sanitize_redacted_payloads::Migration),
            Box::new(m20260825_000061_clear_redacted_poll_choices::Migration),
            Box::new(m20260826_000062_projection_repairs::Migration),
            Box::new(m20260826_000063_poll_answer_selections::Migration),
        ]
    }
}
