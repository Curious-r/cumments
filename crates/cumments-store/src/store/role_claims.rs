use super::DbStore;
use crate::entities::role_claims;
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use cumments_core::governance::{NewRoleClaim, RoleClaim, RoleClaimStatus};
use cumments_core::ports::RoleClaimStore;
use sea_orm::{
    ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};

fn to_claim(model: role_claims::Model) -> Result<RoleClaim> {
    Ok(RoleClaim {
        id: model.id,
        site_id: model.site_id,
        room_id: model.room_id,
        dm_room_id: model.dm_room_id,
        user_id: model.user_id,
        level: model.level,
        token_hash: model.token_hash,
        status: model
            .status
            .parse()
            .map_err(|e: String| anyhow::anyhow!(e))?,
        expires_at: model.expires_at,
        created_at: model.created_at,
        activated_at: model.activated_at,
        applied_at: model.applied_at,
    })
}

#[async_trait]
impl RoleClaimStore for DbStore {
    async fn upsert_role_claim(&self, claim: &NewRoleClaim) -> Result<RoleClaim> {
        let now = Utc::now();
        let model = role_claims::ActiveModel {
            site_id: Set(claim.site_id.clone()),
            room_id: Set(claim.room_id.clone()),
            dm_room_id: Set(None),
            user_id: Set(claim.user_id.clone()),
            level: Set(claim.level),
            token_hash: Set(claim.token_hash.clone()),
            status: Set(RoleClaimStatus::Pending.as_str().to_string()),
            expires_at: Set(claim.expires_at),
            created_at: Set(now),
            activated_at: Set(None),
            applied_at: Set(None),
            ..Default::default()
        };
        role_claims::Entity::insert(model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::columns([
                    role_claims::Column::SiteId,
                    role_claims::Column::RoomId,
                    role_claims::Column::UserId,
                    role_claims::Column::Level,
                ])
                .update_columns([
                    role_claims::Column::TokenHash,
                    role_claims::Column::Status,
                    role_claims::Column::ExpiresAt,
                    role_claims::Column::DmRoomId,
                    role_claims::Column::ActivatedAt,
                    role_claims::Column::AppliedAt,
                ])
                .to_owned(),
            )
            .exec(&self.db)
            .await?;

        let row = role_claims::Entity::find()
            .filter(role_claims::Column::SiteId.eq(&claim.site_id))
            .filter(role_claims::Column::RoomId.eq(&claim.room_id))
            .filter(role_claims::Column::UserId.eq(&claim.user_id))
            .filter(role_claims::Column::Level.eq(claim.level))
            .one(&self.db)
            .await?
            .ok_or_else(|| anyhow::anyhow!("role claim insert did not produce a row"))?;
        to_claim(row)
    }

    async fn pending_claims_for_user(&self, user_id: &str) -> Result<Vec<RoleClaim>> {
        let rows = role_claims::Entity::find()
            .filter(role_claims::Column::UserId.eq(user_id))
            .filter(role_claims::Column::Status.eq(RoleClaimStatus::Pending.as_str()))
            .filter(role_claims::Column::ExpiresAt.gt(Utc::now()))
            .all(&self.db)
            .await?;
        rows.into_iter().map(to_claim).collect()
    }

    async fn mark_claim_activated(&self, id: i64) -> Result<bool> {
        let result = role_claims::Entity::update_many()
            .col_expr(
                role_claims::Column::Status,
                sea_orm::sea_query::Expr::value(RoleClaimStatus::Activated.as_str()),
            )
            .col_expr(
                role_claims::Column::ActivatedAt,
                sea_orm::sea_query::Expr::value(Some(Utc::now())),
            )
            .filter(role_claims::Column::Id.eq(id))
            .filter(role_claims::Column::Status.eq(RoleClaimStatus::Pending.as_str()))
            .filter(role_claims::Column::ExpiresAt.gt(Utc::now()))
            .exec(&self.db)
            .await?;
        Ok(result.rows_affected > 0)
    }

    async fn activated_unapplied_claims(&self) -> Result<Vec<RoleClaim>> {
        let rows = role_claims::Entity::find()
            .filter(role_claims::Column::Status.eq(RoleClaimStatus::Activated.as_str()))
            .filter(role_claims::Column::ExpiresAt.gt(Utc::now()))
            .order_by_asc(role_claims::Column::Id)
            .all(&self.db)
            .await?;
        rows.into_iter().map(to_claim).collect()
    }

    async fn mark_claim_applied(&self, id: i64) -> Result<()> {
        role_claims::Entity::update_many()
            .col_expr(
                role_claims::Column::Status,
                sea_orm::sea_query::Expr::value(RoleClaimStatus::Applied.as_str()),
            )
            .col_expr(
                role_claims::Column::AppliedAt,
                sea_orm::sea_query::Expr::value(Some(Utc::now())),
            )
            .filter(role_claims::Column::Id.eq(id))
            .filter(role_claims::Column::Status.eq(RoleClaimStatus::Activated.as_str()))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn revoke_role_claim(
        &self,
        site_id: &str,
        room_id: &str,
        user_id: &str,
        level: i64,
    ) -> Result<bool> {
        let result = role_claims::Entity::update_many()
            .col_expr(
                role_claims::Column::Status,
                sea_orm::sea_query::Expr::value(RoleClaimStatus::Revoked.as_str()),
            )
            .filter(role_claims::Column::SiteId.eq(site_id))
            .filter(role_claims::Column::RoomId.eq(room_id))
            .filter(role_claims::Column::UserId.eq(user_id))
            .filter(role_claims::Column::Level.eq(level))
            .filter(role_claims::Column::Status.is_in([
                RoleClaimStatus::Pending.as_str(),
                RoleClaimStatus::Activated.as_str(),
            ]))
            .exec(&self.db)
            .await?;
        Ok(result.rows_affected > 0)
    }

    async fn mark_applied_claim_revoked(
        &self,
        site_id: &str,
        room_id: &str,
        user_id: &str,
        level: i64,
    ) -> Result<bool> {
        let result = role_claims::Entity::update_many()
            .col_expr(
                role_claims::Column::Status,
                sea_orm::sea_query::Expr::value(RoleClaimStatus::Revoked.as_str()),
            )
            .filter(role_claims::Column::SiteId.eq(site_id))
            .filter(role_claims::Column::RoomId.eq(room_id))
            .filter(role_claims::Column::UserId.eq(user_id))
            .filter(role_claims::Column::Level.eq(level))
            .filter(role_claims::Column::Status.eq(RoleClaimStatus::Applied.as_str()))
            .exec(&self.db)
            .await?;
        Ok(result.rows_affected > 0)
    }

    async fn list_applied_claims(&self) -> Result<Vec<RoleClaim>> {
        let rows = role_claims::Entity::find()
            .filter(role_claims::Column::Status.eq(RoleClaimStatus::Applied.as_str()))
            .order_by_asc(role_claims::Column::Id)
            .all(&self.db)
            .await?;
        rows.into_iter().map(to_claim).collect()
    }

    async fn set_claim_dm_room_for_user(&self, user_id: &str, room_id: &str) -> Result<()> {
        role_claims::Entity::update_many()
            .col_expr(
                role_claims::Column::DmRoomId,
                sea_orm::sea_query::Expr::value(room_id.to_owned()),
            )
            .filter(role_claims::Column::UserId.eq(user_id))
            .filter(role_claims::Column::Status.eq(RoleClaimStatus::Pending.as_str()))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn claim_dm_room_exists(&self, room_id: &str) -> Result<bool> {
        let count = role_claims::Entity::find()
            .filter(role_claims::Column::DmRoomId.eq(room_id))
            .count(&self.db)
            .await?;
        Ok(count > 0)
    }

    async fn claim_dm_rooms(&self) -> Result<Vec<(String, String)>> {
        let rows: Vec<(String, Option<String>)> = role_claims::Entity::find()
            .select_only()
            .column(role_claims::Column::UserId)
            .column(role_claims::Column::DmRoomId)
            .distinct()
            .filter(role_claims::Column::DmRoomId.is_not_null())
            .into_tuple()
            .all(&self.db)
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(user_id, room_id)| room_id.map(|room_id| (user_id, room_id)))
            .collect())
    }

    async fn active_claims_in_dm_room(&self, user_id: &str, room_id: &str) -> Result<bool> {
        let count = role_claims::Entity::find()
            .filter(role_claims::Column::UserId.eq(user_id))
            .filter(role_claims::Column::DmRoomId.eq(room_id))
            .filter(role_claims::Column::Status.is_in([
                RoleClaimStatus::Pending.as_str(),
                RoleClaimStatus::Activated.as_str(),
            ]))
            .filter(role_claims::Column::ExpiresAt.gt(Utc::now()))
            .count(&self.db)
            .await?;
        Ok(count > 0)
    }

    async fn purge_expired_claims(&self) -> Result<u64> {
        let result = role_claims::Entity::delete_many()
            .filter(role_claims::Column::ExpiresAt.lt(Utc::now()))
            .filter(role_claims::Column::Status.ne(RoleClaimStatus::Applied.as_str()))
            .exec(&self.db)
            .await?;
        Ok(result.rows_affected)
    }
}
