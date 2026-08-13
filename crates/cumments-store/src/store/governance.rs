use super::DbStore;
use crate::entities::{room_roles, site_roles};
use anyhow::Result;
use async_trait::async_trait;
use cumments_core::governance::RoleEntry;
use cumments_core::ports::GovernanceStore;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set, TransactionTrait};

#[async_trait]
impl GovernanceStore for DbStore {
    async fn replace_site_roles(&self, site_id: &str, roles: &[RoleEntry]) -> Result<()> {
        let txn = self.db.begin().await?;
        site_roles::Entity::delete_many()
            .filter(site_roles::Column::SiteId.eq(site_id))
            .exec(&txn)
            .await?;
        if !roles.is_empty() {
            let now = chrono::Utc::now();
            let models = roles.iter().map(|role| site_roles::ActiveModel {
                site_id: Set(site_id.to_string()),
                user_id: Set(role.user_id.clone()),
                level: Set(role.level),
                updated_at: Set(now),
            });
            site_roles::Entity::insert_many(models).exec(&txn).await?;
        }
        txn.commit().await?;
        Ok(())
    }

    async fn list_site_roles(&self, site_id: &str) -> Result<Vec<RoleEntry>> {
        let rows = site_roles::Entity::find()
            .filter(site_roles::Column::SiteId.eq(site_id))
            .order_by_asc(site_roles::Column::UserId)
            .all(&self.db)
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| RoleEntry {
                user_id: row.user_id,
                level: row.level,
            })
            .collect())
    }

    async fn replace_room_roles(&self, room_id: &str, roles: &[RoleEntry]) -> Result<()> {
        let txn = self.db.begin().await?;
        room_roles::Entity::delete_many()
            .filter(room_roles::Column::RoomId.eq(room_id))
            .exec(&txn)
            .await?;
        if !roles.is_empty() {
            let now = chrono::Utc::now();
            let models = roles.iter().map(|role| room_roles::ActiveModel {
                room_id: Set(room_id.to_string()),
                user_id: Set(role.user_id.clone()),
                level: Set(role.level),
                updated_at: Set(now),
            });
            room_roles::Entity::insert_many(models).exec(&txn).await?;
        }
        txn.commit().await?;
        Ok(())
    }

    async fn list_room_roles(&self, room_id: &str) -> Result<Vec<RoleEntry>> {
        let rows = room_roles::Entity::find()
            .filter(room_roles::Column::RoomId.eq(room_id))
            .order_by_asc(room_roles::Column::UserId)
            .all(&self.db)
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| RoleEntry {
                user_id: row.user_id,
                level: row.level,
            })
            .collect())
    }
}
