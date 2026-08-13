//! Final local cleanup for a decommissioned site.
//!
//! Every statement is idempotent so a crashed/retried pass converges. The
//! caller must have retired the Matrix side first: this module only removes
//! local state and must never resurrect a Space through `backfill`.

use anyhow::Result;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter, Statement, Value,
};

use crate::entities::{delete_submissions, post_submissions, room_registry};

/// Deletes every local trace of `site_id`, in dependency order.
pub(crate) async fn delete_site(db: &DatabaseConnection, site_id: &str) -> Result<()> {
    let room_ids = room_registry::Entity::find()
        .filter(room_registry::Column::SiteId.eq(site_id))
        .all(db)
        .await?
        .into_iter()
        .map(|room| room.room_id)
        .collect::<Vec<_>>();

    // Room-scoped rows, then messages, then the registry row.
    for (table, column) in [
        ("room_roles", "room_id"),
        ("room_members", "room_id"),
        ("room_state_events", "room_id"),
        ("backfill_cursors", "room_id"),
        ("backfill_tombstones", "room_id"),
    ] {
        delete_by_values(db, table, column, &room_ids).await?;
    }

    // Submission rows whose payload names this site. Post/delete submissions carry
    // the site only inside their JSON payload; update submissions have a
    // denormalized column.
    delete_post_submissions_for_site(db, site_id).await?;
    delete_delete_submissions_for_site(db, site_id).await?;
    exec(
        db,
        "DELETE FROM update_submissions WHERE site_id = ?",
        vec![site_id.into()],
    )
    .await?;

    // Message-derived rows must go before their parent messages.
    for (table, column) in [
        ("reactions", "message_event_id"),
        ("message_revisions", "message_event_id"),
        ("poll_responses", "poll_message_id"),
    ] {
        exec(
            db,
            &format!(
                "DELETE FROM {table} WHERE {column} IN \
                 (SELECT event_id FROM messages WHERE site_id = ?)"
            ),
            vec![site_id.into()],
        )
        .await?;
    }
    exec(
        db,
        "DELETE FROM messages WHERE site_id = ?",
        vec![site_id.into()],
    )
    .await?;
    delete_by_values(db, "room_registry", "site_id", &[site_id.to_string()]).await?;

    // Site-level rows, ending with the sites row itself.
    for table in [
        "role_claims",
        "site_roles",
        "verification_tokens",
        "site_verified_origins",
        "media_uploads",
        "virtual_users",
    ] {
        exec(
            db,
            &format!("DELETE FROM {table} WHERE site_id = ?"),
            vec![site_id.into()],
        )
        .await?;
    }
    exec(db, "DELETE FROM sites WHERE id = ?", vec![site_id.into()]).await?;
    Ok(())
}

/// Deletes post submissions whose serialized payload names `site_id`.
async fn delete_post_submissions_for_site(db: &DatabaseConnection, site_id: &str) -> Result<()> {
    let models = post_submissions::Entity::find().all(db).await?;
    let mut ids = Vec::new();
    for model in models {
        if payload_site(&model.payload).is_some_and(|id| id == site_id) {
            ids.push(model.id);
        }
    }
    if ids.is_empty() {
        return Ok(());
    }
    post_submissions::Entity::delete_many()
        .filter(post_submissions::Column::Id.is_in(ids))
        .exec(db)
        .await?;
    Ok(())
}

/// Deletes delete submissions whose serialized payload names `site_id`.
async fn delete_delete_submissions_for_site(db: &DatabaseConnection, site_id: &str) -> Result<()> {
    let models = delete_submissions::Entity::find().all(db).await?;
    let mut ids = Vec::new();
    for model in models {
        if payload_site(&model.payload).is_some_and(|id| id == site_id) {
            ids.push(model.id);
        }
    }
    if ids.is_empty() {
        return Ok(());
    }
    delete_submissions::Entity::delete_many()
        .filter(delete_submissions::Column::Id.is_in(ids))
        .exec(db)
        .await?;
    Ok(())
}

fn payload_site(payload: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()?
        .get("site_id")?
        .as_str()
        .map(str::to_owned)
}

/// Deletes rows where `column` is one of `values`, in SQLite-compatible
/// batches. An empty `values` list is a no-op.
async fn delete_by_values(
    db: &DatabaseConnection,
    table: &str,
    column: &str,
    values: &[String],
) -> Result<()> {
    for chunk in values.chunks(500) {
        if chunk.is_empty() {
            continue;
        }
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let bound: Vec<Value> = chunk.iter().map(|value| value.clone().into()).collect();
        exec(
            db,
            &format!("DELETE FROM {table} WHERE {column} IN ({placeholders})"),
            bound,
        )
        .await?;
    }
    Ok(())
}

async fn exec(db: &DatabaseConnection, sql: &str, values: Vec<Value>) -> Result<()> {
    db.execute_raw(Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Sqlite,
        sql,
        values,
    ))
    .await?;
    Ok(())
}
