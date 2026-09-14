//! Database implementation of `MediaReferenceStore` for durable site-scoped
//! media domain mappings between `MediaReference` and Matrix MXC URIs.

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, QueryFilter, Statement, Value,
};

use cumments_core::media_reference::{MediaReference, MediaReferenceSource};
use cumments_core::models::SiteId;
use cumments_core::ports::{MediaReferenceRecord, MediaReferenceResolver, MediaReferenceStore};

use crate::entities::media_references;
use crate::store::DbStore;

fn model_to_record(model: media_references::Model) -> Result<MediaReferenceRecord> {
    let media_reference = MediaReference::parse(&model.media_reference)
        .map_err(|e| anyhow::anyhow!("invalid stored media reference: {e}"))?;
    Ok(MediaReferenceRecord {
        media_reference,
        site_id: SiteId::from(model.site_id),
        mxc_uri: model.mxc_uri,
        is_external: model.is_external,
        created_at: model.created_at,
    })
}

#[async_trait]
impl MediaReferenceResolver for DbStore {
    async fn resolve_mxc(
        &self,
        site_id: &SiteId,
        reference: &MediaReference,
    ) -> Result<Option<String>> {
        let row = media_references::Entity::find()
            .filter(media_references::Column::MediaReference.eq(reference.as_str()))
            .filter(media_references::Column::SiteId.eq(site_id.as_str()))
            .one(&self.db)
            .await?;

        Ok(row.map(|r| r.mxc_uri))
    }
}

#[async_trait]
impl MediaReferenceStore for DbStore {
    async fn find_reference(
        &self,
        site_id: &SiteId,
        mxc_uri: &str,
    ) -> Result<Option<MediaReference>> {
        let row = media_references::Entity::find()
            .filter(media_references::Column::SiteId.eq(site_id.as_str()))
            .filter(media_references::Column::MxcUri.eq(mxc_uri))
            .one(&self.db)
            .await?;

        row.map(|r| {
            MediaReference::parse(&r.media_reference)
                .map_err(|e| anyhow::anyhow!("invalid stored media reference: {e}"))
        })
        .transpose()
    }

    async fn get_or_create_reference(
        &self,
        site_id: &SiteId,
        mxc_uri: &str,
        source: MediaReferenceSource,
    ) -> Result<MediaReference> {
        // Fast path: if mapping already exists, return it without write lock
        if let Some(existing) = self.find_reference(site_id, mxc_uri).await? {
            return Ok(existing);
        }

        let new_reference = MediaReference::new_v4();
        let now = Utc::now();
        let backend = self.db.get_database_backend();

        let sql = if backend == DatabaseBackend::Sqlite {
            "INSERT OR IGNORE INTO media_references \
             (media_reference, site_id, mxc_uri, is_external, created_at) \
             VALUES (?, ?, ?, ?, ?)"
        } else {
            "INSERT INTO media_references \
             (media_reference, site_id, mxc_uri, is_external, created_at) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT (site_id, mxc_uri) DO NOTHING"
        };

        let inserted = self
            .db
            .execute_raw(Statement::from_sql_and_values(
                backend,
                sql,
                vec![
                    Value::from(new_reference.as_str().to_string()),
                    Value::from(site_id.as_str().to_string()),
                    Value::from(mxc_uri.to_string()),
                    Value::from(source.is_external()),
                    Value::from(now),
                ],
            ))
            .await?;

        if inserted.rows_affected() > 0 {
            return Ok(new_reference);
        }

        // Concurrent insert lost race: fetch the winner's mapping
        let winner = self.find_reference(site_id, mxc_uri).await?;
        winner.ok_or_else(|| {
            anyhow::anyhow!(
                "failed to get or create media reference for site '{}' and MXC '{mxc_uri}'",
                site_id.as_str()
            )
        })
    }

    async fn get_record(
        &self,
        site_id: &SiteId,
        reference: &MediaReference,
    ) -> Result<Option<MediaReferenceRecord>> {
        let row = media_references::Entity::find()
            .filter(media_references::Column::MediaReference.eq(reference.as_str()))
            .filter(media_references::Column::SiteId.eq(site_id.as_str()))
            .one(&self.db)
            .await?;

        row.map(model_to_record).transpose()
    }

    async fn get_record_by_mxc(
        &self,
        site_id: &SiteId,
        mxc_uri: &str,
    ) -> Result<Option<MediaReferenceRecord>> {
        let row = media_references::Entity::find()
            .filter(media_references::Column::SiteId.eq(site_id.as_str()))
            .filter(media_references::Column::MxcUri.eq(mxc_uri))
            .one(&self.db)
            .await?;

        row.map(model_to_record).transpose()
    }
}
