//! Database implementation of `ProfileStore` for visitor profile mutations
//! and strict same-field serialization queueing.

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set, Statement,
    TransactionTrait, Value,
};

use cumments_core::models::SiteId;
use cumments_core::ports::ProfileStore;
use cumments_core::profile::{
    ProfileClaimOutcome, ProfileField, ProfileOperation, ProfileOperationStatus, ProfileTargetValue,
};
use cumments_core::submissions::OperationIdentity;

use crate::entities::profile_operations;
use crate::store::DbStore;
use crate::store::submissions::ClaimAttempt;

fn model_to_domain(row: &profile_operations::Model) -> Result<ProfileOperation> {
    let field = ProfileField::from(row.field);
    let status = ProfileOperationStatus::from(row.status);
    let target_value = ProfileTargetValue::from_stored(field, row.target_value.as_deref())
        .map_err(|e| anyhow::anyhow!("failed to parse profile target value: {e}"))?;

    Ok(ProfileOperation {
        operation_id: row.operation_id.clone(),
        author_public_key: row.author_public_key.clone(),
        site_id: SiteId::from(row.site_id.clone()),
        field,
        target_value,
        status,
        sequence: row.sequence,
        response_payload: row.response_payload.clone(),
        error_detail: row.error_detail.clone(),
        created_at: row.created_at,
        updated_at: row.updated_at,
        resolved_at: row.resolved_at,
    })
}

#[async_trait]
impl ProfileStore for DbStore {
    async fn claim_or_get_profile_operation(
        &self,
        operation_id: &str,
        author_public_key: &str,
        site_id: &SiteId,
        target_value: &ProfileTargetValue,
    ) -> Result<ProfileClaimOutcome> {
        let fingerprint = target_value.semantic_fingerprint(site_id.as_str());
        let operation_identity = OperationIdentity {
            operation_id: operation_id.to_string(),
            author_public_key: author_public_key.to_string(),
            fingerprint: fingerprint.clone(),
        };

        let txn = self.db.begin().await?;
        match Self::try_claim_operation(&txn, &operation_identity).await? {
            ClaimAttempt::Claimed => {
                let field_enum =
                    crate::entities::active_enums::ProfileField::from(target_value.field());

                // Calculate next monotonic sequence for (author_public_key, field)
                let max_seq_row = profile_operations::Entity::find()
                    .filter(profile_operations::Column::AuthorPublicKey.eq(author_public_key))
                    .filter(profile_operations::Column::Field.eq(field_enum))
                    .order_by_desc(profile_operations::Column::Sequence)
                    .one(&txn)
                    .await?;

                let sequence = max_seq_row.map_or(1, |row| row.sequence + 1);
                let now = Utc::now();

                let active_model = profile_operations::ActiveModel {
                    operation_id: Set(operation_id.to_string()),
                    author_public_key: Set(author_public_key.to_string()),
                    site_id: Set(site_id.as_str().to_string()),
                    field: Set(field_enum),
                    target_value: Set(target_value.to_stored()),
                    status: Set(crate::entities::active_enums::ProfileOperationStatus::Pending),
                    sequence: Set(sequence),
                    response_payload: Set(None),
                    error_detail: Set(None),
                    created_at: Set(now),
                    updated_at: Set(now),
                    resolved_at: Set(None),
                    ..Default::default()
                };

                profile_operations::Entity::insert(active_model)
                    .exec(&txn)
                    .await?;

                txn.commit().await?;

                let op = ProfileOperation {
                    operation_id: operation_id.to_string(),
                    author_public_key: author_public_key.to_string(),
                    site_id: site_id.clone(),
                    field: target_value.field(),
                    target_value: target_value.clone(),
                    status: ProfileOperationStatus::Pending,
                    sequence,
                    response_payload: None,
                    error_detail: None,
                    created_at: now,
                    updated_at: now,
                    resolved_at: None,
                };

                Ok(ProfileClaimOutcome::New(op))
            }
            ClaimAttempt::Existing(existing_claim) => {
                if existing_claim.author_public_key != author_public_key
                    || existing_claim.fingerprint != fingerprint
                {
                    txn.rollback().await?;
                    return Ok(ProfileClaimOutcome::Conflict);
                }

                let existing_row = profile_operations::Entity::find()
                    .filter(profile_operations::Column::OperationId.eq(operation_id))
                    .one(&txn)
                    .await?;

                txn.commit().await?;

                match existing_row {
                    Some(row) => {
                        let op = model_to_domain(&row)?;
                        if op.site_id != *site_id {
                            return Ok(ProfileClaimOutcome::Conflict);
                        }
                        Ok(ProfileClaimOutcome::Replay(op))
                    }
                    None => Ok(ProfileClaimOutcome::Conflict),
                }
            }
        }
    }

    async fn get_profile_operation(&self, operation_id: &str) -> Result<Option<ProfileOperation>> {
        let row = profile_operations::Entity::find()
            .filter(profile_operations::Column::OperationId.eq(operation_id))
            .one(&self.db)
            .await?;

        row.map(|r| model_to_domain(&r)).transpose()
    }

    async fn claim_for_execution(&self, operation_id: &str) -> Result<bool> {
        let backend = self.db.get_database_backend();
        let now = Utc::now();
        let sql = "UPDATE profile_operations \
                   SET status = 'dispatching', updated_at = ? \
                   WHERE operation_id = ? \
                     AND status = 'pending' \
                     AND NOT EXISTS ( \
                         SELECT 1 FROM profile_operations AS blocking \
                         WHERE blocking.author_public_key = profile_operations.author_public_key \
                           AND blocking.field = profile_operations.field \
                           AND blocking.status IN ('dispatching', 'unknown') \
                           AND blocking.id != profile_operations.id \
                     ) \
                     AND NOT EXISTS ( \
                         SELECT 1 FROM profile_operations AS earlier \
                         WHERE earlier.author_public_key = profile_operations.author_public_key \
                           AND earlier.field = profile_operations.field \
                           AND earlier.sequence < profile_operations.sequence \
                           AND earlier.status = 'pending' \
                     )";
        let res = self
            .db
            .execute_raw(Statement::from_sql_and_values(
                backend,
                sql,
                vec![Value::from(now), Value::from(operation_id.to_string())],
            ))
            .await?;
        Ok(res.rows_affected() > 0)
    }

    async fn record_completed(
        &self,
        operation_id: &str,
        response_payload: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now();
        profile_operations::Entity::update_many()
            .col_expr(
                profile_operations::Column::Status,
                Expr::value(crate::entities::active_enums::ProfileOperationStatus::Completed),
            )
            .col_expr(
                profile_operations::Column::ResponsePayload,
                Expr::value(response_payload.map(str::to_string)),
            )
            .col_expr(
                profile_operations::Column::ResolvedAt,
                Expr::value(Some(now)),
            )
            .col_expr(profile_operations::Column::UpdatedAt, Expr::value(now))
            .filter(profile_operations::Column::OperationId.eq(operation_id))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn record_failed(&self, operation_id: &str, error_detail: &str) -> Result<()> {
        let now = Utc::now();
        profile_operations::Entity::update_many()
            .col_expr(
                profile_operations::Column::Status,
                Expr::value(crate::entities::active_enums::ProfileOperationStatus::Failed),
            )
            .col_expr(
                profile_operations::Column::ErrorDetail,
                Expr::value(Some(error_detail.to_string())),
            )
            .col_expr(
                profile_operations::Column::ResolvedAt,
                Expr::value(Some(now)),
            )
            .col_expr(profile_operations::Column::UpdatedAt, Expr::value(now))
            .filter(profile_operations::Column::OperationId.eq(operation_id))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn record_unknown(&self, operation_id: &str, error_detail: &str) -> Result<()> {
        let now = Utc::now();
        profile_operations::Entity::update_many()
            .col_expr(
                profile_operations::Column::Status,
                Expr::value(crate::entities::active_enums::ProfileOperationStatus::Unknown),
            )
            .col_expr(
                profile_operations::Column::ErrorDetail,
                Expr::value(Some(error_detail.to_string())),
            )
            .col_expr(profile_operations::Column::UpdatedAt, Expr::value(now))
            .filter(profile_operations::Column::OperationId.eq(operation_id))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn record_aborted(&self, operation_id: &str, reason: &str) -> Result<()> {
        let now = Utc::now();
        profile_operations::Entity::update_many()
            .col_expr(
                profile_operations::Column::Status,
                Expr::value(crate::entities::active_enums::ProfileOperationStatus::Aborted),
            )
            .col_expr(
                profile_operations::Column::ErrorDetail,
                Expr::value(Some(reason.to_string())),
            )
            .col_expr(
                profile_operations::Column::ResolvedAt,
                Expr::value(Some(now)),
            )
            .col_expr(profile_operations::Column::UpdatedAt, Expr::value(now))
            .filter(profile_operations::Column::OperationId.eq(operation_id))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn get_next_executable_operation(
        &self,
        author_public_key: &str,
        field: ProfileField,
    ) -> Result<Option<ProfileOperation>> {
        let field_enum = crate::entities::active_enums::ProfileField::from(field);

        // Check if queue for this (author, field) is blocked by Dispatching or Unknown
        let is_blocked = profile_operations::Entity::find()
            .filter(profile_operations::Column::AuthorPublicKey.eq(author_public_key))
            .filter(profile_operations::Column::Field.eq(field_enum))
            .filter(profile_operations::Column::Status.is_in([
                crate::entities::active_enums::ProfileOperationStatus::Dispatching,
                crate::entities::active_enums::ProfileOperationStatus::Unknown,
            ]))
            .one(&self.db)
            .await?
            .is_some();

        if is_blocked {
            return Ok(None);
        }

        let next_pending = profile_operations::Entity::find()
            .filter(profile_operations::Column::AuthorPublicKey.eq(author_public_key))
            .filter(profile_operations::Column::Field.eq(field_enum))
            .filter(
                profile_operations::Column::Status
                    .eq(crate::entities::active_enums::ProfileOperationStatus::Pending),
            )
            .order_by_asc(profile_operations::Column::Sequence)
            .one(&self.db)
            .await?;

        next_pending.map(|r| model_to_domain(&r)).transpose()
    }

    async fn list_operations_for_field(
        &self,
        author_public_key: &str,
        field: ProfileField,
    ) -> Result<Vec<ProfileOperation>> {
        let field_enum = crate::entities::active_enums::ProfileField::from(field);
        let rows = profile_operations::Entity::find()
            .filter(profile_operations::Column::AuthorPublicKey.eq(author_public_key))
            .filter(profile_operations::Column::Field.eq(field_enum))
            .order_by_asc(profile_operations::Column::Sequence)
            .all(&self.db)
            .await?;

        rows.iter().map(model_to_domain).collect()
    }

    async fn recover_crashed_dispatching(&self) -> Result<u64> {
        let now = Utc::now();
        let res = profile_operations::Entity::update_many()
            .col_expr(
                profile_operations::Column::Status,
                Expr::value(crate::entities::active_enums::ProfileOperationStatus::Unknown),
            )
            .col_expr(
                profile_operations::Column::ErrorDetail,
                Expr::value(Some("process crashed during dispatching")),
            )
            .col_expr(profile_operations::Column::UpdatedAt, Expr::value(now))
            .filter(
                profile_operations::Column::Status
                    .eq(crate::entities::active_enums::ProfileOperationStatus::Dispatching),
            )
            .exec(&self.db)
            .await?;
        Ok(res.rows_affected)
    }

    async fn list_executable_pending_operations(
        &self,
        limit: u64,
    ) -> Result<Vec<ProfileOperation>> {
        let backend = self.db.get_database_backend();
        let sql = "SELECT operation_id FROM profile_operations \
                   WHERE status = 'pending' \
                     AND NOT EXISTS ( \
                         SELECT 1 FROM profile_operations AS blocking \
                         WHERE blocking.author_public_key = profile_operations.author_public_key \
                           AND blocking.field = profile_operations.field \
                           AND blocking.status IN ('dispatching', 'unknown') \
                           AND blocking.id != profile_operations.id \
                     ) \
                     AND NOT EXISTS ( \
                         SELECT 1 FROM profile_operations AS earlier \
                         WHERE earlier.author_public_key = profile_operations.author_public_key \
                           AND earlier.field = profile_operations.field \
                           AND earlier.sequence < profile_operations.sequence \
                           AND earlier.status = 'pending' \
                     ) \
                   ORDER BY sequence ASC, created_at ASC \
                   LIMIT ?";
        let rows = self
            .db
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                sql,
                vec![Value::from(limit as i64)],
            ))
            .await?;
        let op_ids = rows
            .iter()
            .filter_map(|row| row.try_get_by_index::<String>(0).ok())
            .collect::<Vec<_>>();

        if op_ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut ops = Vec::new();
        for id in op_ids {
            if let Some(op) = self.get_profile_operation(&id).await? {
                ops.push(op);
            }
        }
        Ok(ops)
    }
}
