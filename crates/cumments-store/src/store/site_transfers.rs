use super::DbStore;
use crate::entities::site_transfers;
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use cumments_core::{
    governance::{SiteTransfer, SiteTransferStatus},
    ports::SiteTransferStore,
};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};

fn to_transfer(model: site_transfers::Model) -> Result<SiteTransfer> {
    Ok(SiteTransfer {
        id: model.id,
        site_id: model.site_id,
        target_mxid: model.target_mxid,
        status: model
            .status
            .parse()
            .map_err(|e: String| anyhow::anyhow!(e))?,
        expires_at: model.expires_at,
        created_at: model.created_at,
        completed_at: model.completed_at,
    })
}

#[async_trait]
impl SiteTransferStore for DbStore {
    async fn upsert_pending_transfer(
        &self,
        site_id: &str,
        target_mxid: &str,
        expires_at: chrono::DateTime<Utc>,
    ) -> Result<SiteTransfer> {
        site_transfers::Entity::update_many()
            .col_expr(
                site_transfers::Column::Status,
                sea_orm::sea_query::Expr::value(SiteTransferStatus::Cancelled.as_str()),
            )
            .filter(site_transfers::Column::SiteId.eq(site_id))
            .filter(site_transfers::Column::Status.eq(SiteTransferStatus::Pending.as_str()))
            .exec(&self.db)
            .await?;

        let model = site_transfers::ActiveModel {
            site_id: Set(site_id.to_string()),
            target_mxid: Set(target_mxid.to_string()),
            status: Set(SiteTransferStatus::Pending.as_str().to_string()),
            expires_at: Set(expires_at),
            created_at: Set(Utc::now()),
            completed_at: Set(None),
            ..Default::default()
        };
        let inserted = site_transfers::Entity::insert(model).exec(&self.db).await?;
        let row = site_transfers::Entity::find_by_id(inserted.last_insert_id)
            .one(&self.db)
            .await?
            .ok_or_else(|| anyhow::anyhow!("site transfer insert did not produce a row"))?;
        to_transfer(row)
    }

    async fn find_pending_transfer(&self, site_id: &str) -> Result<Option<SiteTransfer>> {
        let row = site_transfers::Entity::find()
            .filter(site_transfers::Column::SiteId.eq(site_id))
            .filter(site_transfers::Column::Status.eq(SiteTransferStatus::Pending.as_str()))
            .filter(site_transfers::Column::ExpiresAt.gt(Utc::now()))
            .one(&self.db)
            .await?;
        row.map(to_transfer).transpose()
    }

    async fn complete_transfer(&self, site_id: &str, id: i64) -> Result<bool> {
        let result = site_transfers::Entity::update_many()
            .col_expr(
                site_transfers::Column::Status,
                sea_orm::sea_query::Expr::value(SiteTransferStatus::Completed.as_str()),
            )
            .col_expr(
                site_transfers::Column::CompletedAt,
                sea_orm::sea_query::Expr::value(Some(Utc::now())),
            )
            .filter(site_transfers::Column::Id.eq(id))
            .filter(site_transfers::Column::SiteId.eq(site_id))
            .filter(site_transfers::Column::Status.eq(SiteTransferStatus::Pending.as_str()))
            .exec(&self.db)
            .await?;
        Ok(result.rows_affected > 0)
    }

    async fn expire_pending_transfers(&self) -> Result<u64> {
        let result = site_transfers::Entity::update_many()
            .col_expr(
                site_transfers::Column::Status,
                sea_orm::sea_query::Expr::value(SiteTransferStatus::Expired.as_str()),
            )
            .filter(site_transfers::Column::Status.eq(SiteTransferStatus::Pending.as_str()))
            .filter(site_transfers::Column::ExpiresAt.lte(Utc::now()))
            .exec(&self.db)
            .await?;
        Ok(result.rows_affected)
    }
}
