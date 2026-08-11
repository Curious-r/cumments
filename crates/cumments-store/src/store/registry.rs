use super::DbStore;
use crate::entities::{room_registry, sites};
use anyhow::Result;
use async_trait::async_trait;
use cumments_core::models::{PostSlug, Site, SiteId};
use cumments_core::ports::{RegistryStore, SiteStore};
use sea_orm::{EntityTrait, QueryFilter, Set};

#[async_trait]
impl RegistryStore for DbStore {
    async fn get_registered_room(
        &self,
        site_id: &SiteId,
        post_slug: &PostSlug,
    ) -> Result<Option<String>> {
        let room = room_registry::Entity::find()
            .filter(room_registry::COLUMN.site_id.eq(site_id.as_str()))
            .filter(room_registry::COLUMN.post_slug.eq(post_slug.as_str()))
            .filter(room_registry::COLUMN.is_active.eq(true))
            .one(&self.db)
            .await?;

        Ok(room.map(|r| r.room_id))
    }

    async fn is_room_active(&self, room_id: &str) -> Result<Option<bool>> {
        let room = room_registry::Entity::find_by_id(room_id.to_owned())
            .one(&self.db)
            .await?;

        Ok(room.map(|r| r.is_active))
    }

    async fn get_registered_room_identity(
        &self,
        room_id: &str,
    ) -> Result<Option<(String, String)>> {
        let room = room_registry::Entity::find_by_id(room_id.to_owned())
            .one(&self.db)
            .await?;

        Ok(room.map(|r| (r.site_id, r.post_slug)))
    }

    async fn register_room(
        &self,
        room_id: &str,
        site_id: &SiteId,
        post_slug: &PostSlug,
    ) -> Result<()> {
        let active_model = room_registry::ActiveModel {
            room_id: Set(room_id.to_owned()),
            site_id: Set(site_id.as_str().to_owned()),
            post_slug: Set(post_slug.as_str().to_owned()),
            is_active: Set(true),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
        };

        room_registry::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(room_registry::Column::RoomId)
                    .update_column(room_registry::Column::IsActive)
                    .update_column(room_registry::Column::SiteId)
                    .update_column(room_registry::Column::PostSlug)
                    .update_column(room_registry::Column::UpdatedAt)
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;

        Ok(())
    }

    async fn invalidate_room_registry(&self, room_id: &str) -> Result<()> {
        room_registry::Entity::update_many()
            .col_expr(
                room_registry::Column::IsActive,
                sea_orm::sea_query::Expr::value(false),
            )
            .col_expr(
                room_registry::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(chrono::Utc::now()),
            )
            .filter(room_registry::COLUMN.room_id.eq(room_id))
            .exec(&self.db)
            .await?;
        Ok(())
    }
}

#[async_trait]
impl SiteStore for DbStore {
    async fn get_site(&self, id: &SiteId) -> Result<Option<Site>> {
        let model = sites::Entity::find_by_id(id.as_str().to_owned())
            .one(&self.db)
            .await?;

        Ok(model.map(Site::from))
    }

    async fn get_site_by_space_id(&self, space_id: &str) -> Result<Option<Site>> {
        let model = sites::Entity::find()
            .filter(sites::COLUMN.matrix_space_id.eq(space_id))
            .one(&self.db)
            .await?;

        Ok(model.map(Site::from))
    }

    async fn save_site(&self, site: &Site) -> Result<()> {
        let active_model = sites::ActiveModel {
            id: Set(site.id.clone()),
            matrix_space_id: Set(site.matrix_space_id.clone()),
            display_name: Set(site.display_name.clone()),
            created_at: Set(site.created_at),
        };

        sites::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(sites::Column::Id)
                    .update_columns([sites::Column::MatrixSpaceId, sites::Column::DisplayName])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;

        Ok(())
    }

    async fn ensure_site_exists(&self, site_id: &str, matrix_space_id: &str) -> Result<()> {
        let active_model = sites::ActiveModel {
            id: Set(site_id.to_owned()),
            matrix_space_id: Set(matrix_space_id.to_owned()),
            display_name: Set(Some(site_id.to_owned())),
            created_at: Set(chrono::Utc::now()),
        };

        sites::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(sites::Column::Id)
                    // Upsert instead of do-nothing: with DO NOTHING and no
                    // RETURNING row, sea-orm treats a conflicting existing
                    // site as an insert failure, which made every space-child
                    // push event fail and blocked the homeserver's push queue.
                    .update_columns([sites::Column::MatrixSpaceId, sites::Column::DisplayName])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;

        Ok(())
    }
}

impl From<sites::Model> for Site {
    fn from(model: sites::Model) -> Self {
        Site {
            id: model.id,
            matrix_space_id: model.matrix_space_id,
            display_name: model.display_name,
            created_at: model.created_at,
        }
    }
}
