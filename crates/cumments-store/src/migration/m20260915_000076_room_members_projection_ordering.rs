use sea_orm::Statement;
use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

#[derive(DeriveMigrationName)]
pub struct Migration;

fn parse_timestamp_millis(s: &str) -> Option<i64> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp_millis());
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f") {
        return Some(dt.and_utc().timestamp_millis());
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return Some(dt.and_utc().timestamp_millis());
    }
    None
}

/// Add `origin_server_ts` and nullable `event_id` columns to `room_members` projection table.
///
/// These columns record the deterministic projection ordering version for monotonic
/// application ordering, preventing late-arriving or out-of-order member events from
/// regressing the current room presentation projection.
///
/// Legacy rows are migrated by recovering real member event identity from `room_state_events`
/// when reliably identifiable, or falling back conservatively to `updated_at` with unknown event ID.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !column_exists(manager, "room_members", "origin_server_ts").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("room_members"))
                        .add_column(
                            ColumnDef::new(Alias::new("origin_server_ts"))
                                .big_integer()
                                .not_null()
                                .default(0),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !column_exists(manager, "room_members", "event_id").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("room_members"))
                        .add_column(ColumnDef::new(Alias::new("event_id")).string().null())
                        .to_owned(),
                )
                .await?;
        }

        let db = manager.get_connection();
        let backend = manager.get_database_backend();

        let has_state_table = column_exists(manager, "room_state_events", "event_id").await?;

        // Select all legacy room_members rows needing projection version resolution.
        let select_sql = "SELECT room_id, user_id, membership, display_name, avatar_url, updated_at, origin_server_ts, \
                          CAST((strftime('%s', updated_at) * 1000) AS INTEGER) AS fallback_ts \
                          FROM room_members \
                          WHERE event_id IS NULL";
        let legacy_rows = db
            .query_all_raw(Statement::from_string(backend, select_sql.to_string()))
            .await?;

        for row in legacy_rows {
            let room_id: String = row.try_get("", "room_id")?;
            let user_id: String = row.try_get("", "user_id")?;
            let membership: String = row.try_get("", "membership")?;
            let display_name: Option<String> = row.try_get("", "display_name")?;
            let avatar_url: Option<String> = row.try_get("", "avatar_url")?;
            let updated_at_str: Option<String> = row.try_get("", "updated_at")?;
            let current_origin_ts: i64 = row.try_get("", "origin_server_ts").unwrap_or(0);
            let fallback_ts: i64 = row.try_get("", "fallback_ts").unwrap_or(0);

            let precise_ts = updated_at_str
                .as_deref()
                .and_then(parse_timestamp_millis)
                .unwrap_or(fallback_ts);

            // Baseline timestamp to use if event cannot be recovered
            let baseline_ts = if current_origin_ts > 0 {
                current_origin_ts
            } else {
                precise_ts
            };

            let mut resolved_ts = baseline_ts;
            let mut resolved_event_id: Option<String> = None;

            if has_state_table {
                let cand_sql = "SELECT event_id, origin_server_ts, content_json \
                                FROM room_state_events \
                                WHERE room_id = ? AND state_key = ? AND event_type = 'm.room.member' \
                                ORDER BY origin_server_ts DESC, event_id DESC";
                let cand_rows = db
                    .query_all_raw(Statement::from_sql_and_values(
                        backend,
                        cand_sql,
                        [room_id.clone().into(), user_id.clone().into()],
                    ))
                    .await?;

                let mut matching_candidates = Vec::new();
                for cand_row in cand_rows {
                    let eid: String = cand_row.try_get("", "event_id")?;
                    let ts: i64 = cand_row.try_get("", "origin_server_ts")?;
                    let content_json: String = cand_row.try_get("", "content_json")?;
                    let val: serde_json::Value =
                        serde_json::from_str(&content_json).unwrap_or(serde_json::Value::Null);
                    let cand_mem = val.get("membership").and_then(|v| v.as_str()).unwrap_or("");
                    let cand_dname = val.get("displayname").and_then(|v| v.as_str());
                    let cand_avatar = val.get("avatar_url").and_then(|v| v.as_str());

                    if cand_mem == membership {
                        matching_candidates.push((
                            eid,
                            ts,
                            cand_dname.map(str::to_string),
                            cand_avatar.map(str::to_string),
                        ));
                    }
                }

                if !matching_candidates.is_empty() {
                    // Step 1: Match by exact timestamp if baseline_ts is positive.
                    let ts_matches: Vec<_> = matching_candidates
                        .iter()
                        .filter(|(_, ts, _, _)| *ts == baseline_ts || *ts == precise_ts)
                        .collect();

                    if ts_matches.len() == 1 {
                        resolved_ts = ts_matches[0].1;
                        resolved_event_id = Some(ts_matches[0].0.clone());
                    } else if matching_candidates.len() == 1 {
                        // Step 2: Exactly one member event with matching membership in the room's history.
                        resolved_ts = matching_candidates[0].1;
                        resolved_event_id = Some(matching_candidates[0].0.clone());
                    } else if membership == "join" {
                        // Step 3: For join, filter by matching presentation (displayname and avatar).
                        let pres_matches: Vec<_> = matching_candidates
                            .iter()
                            .filter(|(_, _, d, a)| {
                                d.as_deref() == display_name.as_deref()
                                    && a.as_deref() == avatar_url.as_deref()
                            })
                            .collect();
                        if pres_matches.len() == 1 {
                            resolved_ts = pres_matches[0].1;
                            resolved_event_id = Some(pres_matches[0].0.clone());
                        }
                    }
                }
            }

            let update_sql = "UPDATE room_members \
                              SET origin_server_ts = ?, event_id = ? \
                              WHERE room_id = ? AND user_id = ?";
            db.execute_raw(Statement::from_sql_and_values(
                backend,
                update_sql,
                [
                    resolved_ts.into(),
                    resolved_event_id.into(),
                    room_id.into(),
                    user_id.into(),
                ],
            ))
            .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if column_exists(manager, "room_members", "event_id").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("room_members"))
                        .drop_column(Alias::new("event_id"))
                        .to_owned(),
                )
                .await?;
        }

        if column_exists(manager, "room_members", "origin_server_ts").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("room_members"))
                        .drop_column(Alias::new("origin_server_ts"))
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
}
