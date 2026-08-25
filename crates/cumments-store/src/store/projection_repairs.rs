use super::DbStore;
use crate::entities::projection_repairs;
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use cumments_core::models::{ProjectionRepair, ProjectionRepairInput, ProjectionRepairStatus};
use cumments_core::ports::ProjectionRepairStore;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};

/// Consecutive repair-pass failures before automatic retries stop. A short
/// budget keeps a permanently missing/unreadable event from spinning while
/// still tolerating homeserver restarts and brief read races.
const MAX_AUTOMATIC_ATTEMPTS: u32 = 5;

fn model_from_row(row: projection_repairs::Model) -> Result<ProjectionRepair, anyhow::Error> {
    let status = row.status()?;
    Ok(ProjectionRepair {
        target_event_id: row.target_event_id,
        room_id: row.room_id,
        redaction_event_id: row.redaction_event_id,
        reason: row.reason,
        observed_room_version: row.observed_room_version,
        status,
        attempts: row.attempts,
        last_error: row.last_error,
        next_retry_at: row.next_retry_at,
        created_at: row.created_at,
        updated_at: row.updated_at,
        resolved_at: row.resolved_at,
    })
}

#[async_trait]
impl ProjectionRepairStore for DbStore {
    async fn record_projection_repair(&self, input: &ProjectionRepairInput) -> Result<()> {
        let now = Utc::now();
        let active = projection_repairs::ActiveModel {
            id: sea_orm::ActiveValue::NotSet,
            target_event_id: Set(input.target_event_id.clone()),
            room_id: Set(input.room_id.clone()),
            redaction_event_id: Set(input.redaction_event_id.clone()),
            reason: Set(input.reason.to_owned()),
            observed_room_version: Set(input.observed_room_version.clone()),
            status: Set(ProjectionRepairStatus::Pending.as_str().to_owned()),
            attempts: Set(1),
            last_error: Set(Some(input.error.clone())),
            next_retry_at: Set(now),
            created_at: Set(now),
            updated_at: Set(now),
            resolved_at: Set(None),
        };

        projection_repairs::Entity::insert(active)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(projection_repairs::Column::TargetEventId)
                    .update_columns([
                        projection_repairs::Column::RoomId,
                        projection_repairs::Column::RedactionEventId,
                        projection_repairs::Column::Reason,
                        projection_repairs::Column::ObservedRoomVersion,
                        projection_repairs::Column::Status,
                        projection_repairs::Column::LastError,
                        projection_repairs::Column::NextRetryAt,
                        projection_repairs::Column::UpdatedAt,
                        projection_repairs::Column::ResolvedAt,
                    ])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn claim_due_projection_repairs(&self, limit: u64) -> Result<Vec<ProjectionRepair>> {
        let rows = projection_repairs::Entity::find()
            .filter(projection_repairs::Column::Status.eq(ProjectionRepairStatus::Pending.as_str()))
            .filter(projection_repairs::Column::NextRetryAt.lte(Utc::now()))
            .order_by_asc(projection_repairs::Column::NextRetryAt)
            .limit(limit)
            .all(&self.db)
            .await?;
        rows.into_iter().map(model_from_row).collect()
    }

    async fn record_projection_repair_failure(
        &self,
        target_event_id: &str,
        error: &str,
        retry_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        let Some(row) = projection_repairs::Entity::find()
            .filter(projection_repairs::Column::TargetEventId.eq(target_event_id))
            .one(&self.db)
            .await?
        else {
            anyhow::bail!("projection repair {target_event_id} not found");
        };

        let attempts = row.attempts.saturating_add(1);
        let status = if attempts >= MAX_AUTOMATIC_ATTEMPTS {
            ProjectionRepairStatus::Manual
        } else {
            ProjectionRepairStatus::Pending
        };
        let mut active: projection_repairs::ActiveModel = row.into();
        active.attempts = Set(attempts);
        active.status = Set(status.as_str().to_owned());
        active.last_error = Set(Some(error.to_owned()));
        active.next_retry_at = Set(retry_at);
        active.updated_at = Set(Utc::now());
        active.update(&self.db).await?;
        Ok(())
    }

    async fn mark_projection_repair_manual(
        &self,
        target_event_id: &str,
        error: &str,
    ) -> Result<()> {
        let result = projection_repairs::Entity::update_many()
            .col_expr(
                projection_repairs::Column::Status,
                sea_orm::sea_query::Expr::value(ProjectionRepairStatus::Manual.as_str()),
            )
            .col_expr(
                projection_repairs::Column::LastError,
                sea_orm::sea_query::Expr::value(Some(error.to_owned())),
            )
            .col_expr(
                projection_repairs::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(Utc::now()),
            )
            .filter(projection_repairs::Column::TargetEventId.eq(target_event_id))
            .exec(&self.db)
            .await?;
        if result.rows_affected == 0 {
            anyhow::bail!("projection repair {target_event_id} not found");
        }
        Ok(())
    }

    async fn resolve_projection_repair(&self, target_event_id: &str) -> Result<bool> {
        let result = projection_repairs::Entity::update_many()
            .col_expr(
                projection_repairs::Column::Status,
                sea_orm::sea_query::Expr::value(ProjectionRepairStatus::Resolved.as_str()),
            )
            .col_expr(
                projection_repairs::Column::ResolvedAt,
                sea_orm::sea_query::Expr::value(Some(Utc::now())),
            )
            .col_expr(
                projection_repairs::Column::LastError,
                sea_orm::sea_query::Expr::value(Option::<String>::None),
            )
            .col_expr(
                projection_repairs::Column::NextRetryAt,
                sea_orm::sea_query::Expr::value(Utc::now()),
            )
            .filter(projection_repairs::Column::TargetEventId.eq(target_event_id))
            .exec(&self.db)
            .await?;
        Ok(result.rows_affected > 0)
    }

    async fn list_projection_repairs(
        &self,
        status: Option<ProjectionRepairStatus>,
        limit: u64,
    ) -> Result<Vec<ProjectionRepair>> {
        let mut query = projection_repairs::Entity::find()
            .order_by_desc(projection_repairs::Column::UpdatedAt)
            .limit(limit);
        if let Some(status) = status {
            query = query.filter(projection_repairs::Column::Status.eq(status.as_str()));
        }
        let rows = query.all(&self.db).await?;
        rows.into_iter().map(model_from_row).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(target: &str) -> ProjectionRepairInput {
        ProjectionRepairInput {
            target_event_id: target.to_owned(),
            room_id: "!room:hs".to_owned(),
            redaction_event_id: "$red:hs".to_owned(),
            reason: "unsupported_room_version",
            observed_room_version: Some("custom-experimental".to_owned()),
            error: "test failure".to_owned(),
        }
    }

    #[tokio::test]
    async fn repair_rows_are_durable_bounded_and_resolvable() {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "cumments-projection-repairs-{}-{id}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = DbStore::connect(&format!("sqlite://{}", path.display()))
            .await
            .expect("connect db");

        store
            .record_projection_repair(&input("$target:hs"))
            .await
            .expect("record repair");
        let claimed = store
            .claim_due_projection_repairs(10)
            .await
            .expect("claim due");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].attempts, 1);

        for attempt in 1..5u32 {
            store
                .record_projection_repair_failure(
                    "$target:hs",
                    &format!("failure {attempt}"),
                    Utc::now() + chrono::Duration::seconds(60),
                )
                .await
                .expect("record failure");
        }
        assert!(
            store
                .claim_due_projection_repairs(10)
                .await
                .expect("claim future")
                .is_empty(),
            "a scheduled repair must not be claimed early"
        );

        let rows = store
            .list_projection_repairs(Some(ProjectionRepairStatus::Manual), 10)
            .await
            .expect("list manual");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].attempts, 5);

        assert!(
            store
                .resolve_projection_repair("$target:hs")
                .await
                .expect("resolve"),
            "repair must be resolvable"
        );
        assert!(
            store
                .list_projection_repairs(Some(ProjectionRepairStatus::Pending), 10)
                .await
                .expect("pending rows")
                .is_empty()
        );
        let _ = std::fs::remove_file(&path);
    }
}
