use super::DbStore;
use crate::entities::virtual_users;
use anyhow::Result;
use async_trait::async_trait;
use cumments_core::identity::derive_guest_id_from_public_key;
use cumments_core::models::SiteId;
use cumments_core::ports::VirtualUserStore;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};

#[async_trait]
impl VirtualUserStore for DbStore {
    async fn get_or_create_virtual_user(
        &self,
        author_public_key: &str,
        site_id: &SiteId,
        server_name: &str,
    ) -> Result<String> {
        // 1. Compute the deterministic virtual user ID
        let guest_id = derive_guest_id_from_public_key(author_public_key)
            .ok_or_else(|| anyhow::anyhow!("invalid author public key"))?;
        let virtual_user_id = format!(
            "@_cumments_{}_{}:{}",
            site_id.as_str(),
            guest_id,
            server_name
        );

        // The mapping is stable per (public key, site): return the stored
        // virtual user even when the current server_name differs (e.g. after
        // a domain migration), so edits keep matching the original sender.
        if let Some(existing) = virtual_users::Entity::find()
            .filter(virtual_users::Column::PublicKey.eq(author_public_key))
            .filter(virtual_users::Column::SiteId.eq(site_id.as_str()))
            .one(&self.db)
            .await?
        {
            return Ok(existing.virtual_user_id);
        }

        // 2. Try to insert – on conflict (public_key + site_id already exists), do nothing
        let active_model = virtual_users::ActiveModel {
            public_key: Set(author_public_key.to_owned()),
            site_id: Set(site_id.as_str().to_owned()),
            virtual_user_id: Set(virtual_user_id.clone()),
            server_name: Set(server_name.to_owned()),
            created_at: Set(chrono::Utc::now()),
        };

        virtual_users::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::columns([
                    virtual_users::Column::PublicKey,
                    virtual_users::Column::SiteId,
                ])
                .do_nothing()
                .to_owned(),
            )
            .exec(&self.db)
            .await?;

        // Re-read after the insert: a concurrent request may have won the
        // race, and the winner's stored ID is authoritative.
        if let Some(existing) = virtual_users::Entity::find()
            .filter(virtual_users::Column::PublicKey.eq(author_public_key))
            .filter(virtual_users::Column::SiteId.eq(site_id.as_str()))
            .one(&self.db)
            .await?
        {
            return Ok(existing.virtual_user_id);
        }

        Ok(virtual_user_id)
    }
}
