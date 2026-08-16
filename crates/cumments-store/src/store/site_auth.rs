use super::DbStore;
use super::is_unique_violation;
use crate::entities::active_enums::{
    SiteAuthMode as DbAuthMode, SiteLifecycleStatus as DbLifecycle,
    SiteVerificationStatus as DbVerificationStatus,
};
use crate::entities::{site_verified_origins, sites, verification_tokens};
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use cumments_core::ports::SiteAuthStore;
use cumments_core::site_auth::{
    NewVerificationToken, Origin, SiteAuthInfo, SiteAuthMode, SiteLifecycle, SiteServiceError,
    SiteVerificationStatus, VerificationToken,
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, NotSet, QueryFilter, Set, TransactionTrait,
};
use std::collections::HashMap;

#[async_trait]
impl SiteAuthStore for DbStore {
    async fn register_site(
        &self,
        site_id: &str,
        claim_token_hash: &str,
        custom_id: bool,
    ) -> Result<(), SiteServiceError> {
        let now = Utc::now();
        let model = sites::ActiveModel {
            id: Set(site_id.to_owned()),
            matrix_space_id: Set(String::new()),
            display_name: Set(None),
            auth_mode: Set(DbAuthMode::Origin),
            verification_status: Set(DbVerificationStatus::Unverified),
            claim_token_hash: Set(Some(claim_token_hash.to_owned())),
            is_custom_id: Set(custom_id),
            lifecycle_status: Set(DbLifecycle::Active),
            secret: Set(None),
            verified_at: Set(None),
            created_at: Set(now),
            updated_at: Set(Some(now)),
        };

        match sites::Entity::insert(model).exec(&self.db).await {
            Ok(_) => Ok(()),
            Err(e) if is_unique_violation(&e) => {
                Err(SiteServiceError::SiteAlreadyExists(site_id.to_string()))
            }
            Err(e) => Err(SiteServiceError::Store(anyhow::Error::new(e))),
        }
    }

    async fn get_site_auth(&self, site_id: &str) -> Result<Option<SiteAuthInfo>> {
        let Some(site) = sites::Entity::find_by_id(site_id.to_owned())
            .one(&self.db)
            .await?
        else {
            return Ok(None);
        };

        let origins = site_verified_origins::Entity::find()
            .filter(site_verified_origins::Column::SiteId.eq(site_id))
            .all(&self.db)
            .await?
            .into_iter()
            .map(|row| Origin::parse(&row.origin))
            .collect::<Result<Vec<_>>>()?;

        Ok(Some(SiteAuthInfo {
            site_id: site.id,
            lifecycle: core_lifecycle(site.lifecycle_status),
            is_custom_id: site.is_custom_id,
            auth_mode: core_auth_mode(site.auth_mode),
            verification_status: core_verification_status(site.verification_status),
            verified_origins: origins,
            verified_at: site.verified_at,
            secret: site.secret,
            claim_token_hash: site.claim_token_hash,
            updated_at: site.updated_at,
        }))
    }

    async fn get_claim_token_hash(&self, site_id: &str) -> Result<Option<String>> {
        Ok(sites::Entity::find_by_id(site_id.to_owned())
            .one(&self.db)
            .await?
            .and_then(|site| site.claim_token_hash))
    }

    async fn mark_site_retiring(&self, site_id: &str) -> Result<bool> {
        let result = sites::Entity::update_many()
            .col_expr(
                sites::Column::LifecycleStatus,
                sea_orm::sea_query::Expr::value(lifecycle_value(DbLifecycle::Retiring)),
            )
            .col_expr(
                sites::Column::ClaimTokenHash,
                sea_orm::sea_query::Expr::value(Option::<String>::None),
            )
            .col_expr(
                sites::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(Some(Utc::now())),
            )
            .filter(sites::Column::Id.eq(site_id))
            .filter(sites::Column::LifecycleStatus.eq(lifecycle_value(DbLifecycle::Active)))
            .exec(&self.db)
            .await?;
        Ok(result.rows_affected > 0)
    }

    async fn list_retiring_sites(&self) -> Result<Vec<String>> {
        let rows = sites::Entity::find()
            .filter(sites::Column::LifecycleStatus.eq(lifecycle_value(DbLifecycle::Retiring)))
            .all(&self.db)
            .await?;
        Ok(rows.into_iter().map(|site| site.id).collect())
    }

    async fn delete_site(&self, site_id: &str) -> Result<()> {
        crate::store::decommission::delete_site(&self.db, site_id).await
    }

    async fn delete_room_local(&self, room_id: &str) -> Result<()> {
        crate::store::decommission::delete_room(&self.db, room_id).await
    }

    async fn rotate_claim_token(&self, site_id: &str, new_hash: &str) -> Result<bool> {
        let result = sites::Entity::update_many()
            .col_expr(
                sites::Column::ClaimTokenHash,
                sea_orm::sea_query::Expr::value(Some(new_hash)),
            )
            .col_expr(
                sites::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(Some(Utc::now())),
            )
            .filter(sites::Column::Id.eq(site_id))
            .exec(&self.db)
            .await?;
        Ok(result.rows_affected == 1)
    }

    async fn insert_verification_tokens(&self, tokens: &[NewVerificationToken]) -> Result<()> {
        let now = Utc::now();
        let methods_json = tokens
            .iter()
            .map(|token| {
                serde_json::to_string(&token.methods)
                    .map_err(|e| anyhow::anyhow!("failed to serialize verification methods: {e}"))
            })
            .collect::<Result<Vec<_>>>()?;
        let models = tokens
            .iter()
            .zip(methods_json)
            .map(|(token, methods)| verification_tokens::ActiveModel {
                id: NotSet,
                site_id: Set(token.site_id.clone()),
                origin: Set(token.origin.as_str().to_owned()),
                token_hash: Set(token.token_hash.clone()),
                methods: Set(methods),
                expires_at: Set(token.expires_at),
                consumed_at: Set(None),
                created_at: Set(now),
                attempts: Set(0),
            })
            .collect::<Vec<_>>();
        verification_tokens::Entity::insert_many(models)
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn find_verification_token(
        &self,
        site_id: &str,
        origin: &Origin,
        token_hash: &str,
    ) -> Result<Option<VerificationToken>> {
        // Opportunistic GC: drop expired tokens and consumed tokens older
        // than a day so the table cannot grow without bound.
        self.db
            .execute_unprepared(
                "DELETE FROM verification_tokens \
                 WHERE expires_at < datetime('now') \
                    OR (consumed_at IS NOT NULL AND consumed_at < datetime('now', '-1 day'))",
            )
            .await?;

        let now = Utc::now();
        let row = verification_tokens::Entity::find()
            .filter(verification_tokens::Column::SiteId.eq(site_id))
            .filter(verification_tokens::Column::Origin.eq(origin.as_str()))
            .filter(verification_tokens::Column::TokenHash.eq(token_hash))
            .filter(verification_tokens::Column::ConsumedAt.is_null())
            .filter(verification_tokens::Column::ExpiresAt.gt(now))
            .one(&self.db)
            .await?;
        row.map(core_verification_token).transpose()
    }

    async fn increment_verification_attempt(&self, id: i64) -> Result<u32> {
        self.db
            .execute_unprepared(&format!(
                "UPDATE verification_tokens \
                 SET attempts = attempts + 1 \
                 WHERE id = {id}"
            ))
            .await?;
        let row = verification_tokens::Entity::find_by_id(id)
            .one(&self.db)
            .await?;
        Ok(row.map(|r| r.attempts as u32).unwrap_or(0))
    }

    async fn consume_verification_token(&self, id: i64) -> Result<bool> {
        let result = verification_tokens::Entity::update_many()
            .col_expr(
                verification_tokens::Column::ConsumedAt,
                sea_orm::sea_query::Expr::value(Utc::now()),
            )
            .filter(verification_tokens::Column::Id.eq(id))
            .filter(verification_tokens::Column::ConsumedAt.is_null())
            .exec(&self.db)
            .await?;
        Ok(result.rows_affected == 1)
    }

    async fn add_verified_origin(&self, site_id: &str, origin: &Origin) -> Result<()> {
        let transaction = self.db.begin().await?;

        site_verified_origins::Entity::insert(site_verified_origins::ActiveModel {
            site_id: Set(site_id.to_owned()),
            origin: Set(origin.as_str().to_owned()),
            created_at: Set(Utc::now()),
        })
        .on_conflict(
            sea_orm::sea_query::OnConflict::columns([
                site_verified_origins::Column::SiteId,
                site_verified_origins::Column::Origin,
            ])
            // Upsert instead of DO NOTHING: sea-orm reports a conflicting
            // insert as an error, and idempotent completion must not fail
            // when a concurrent confirmation already recorded the origin.
            .update_column(site_verified_origins::Column::CreatedAt)
            .to_owned(),
        )
        .exec(&transaction)
        .await?;

        let existing_verified_at = sites::Entity::find_by_id(site_id.to_owned())
            .one(&transaction)
            .await?
            .and_then(|site| site.verified_at);
        sites::Entity::update_many()
            .col_expr(
                sites::Column::VerificationStatus,
                sea_orm::sea_query::Expr::value(DbVerificationStatus::Verified),
            )
            .col_expr(
                sites::Column::VerifiedAt,
                sea_orm::sea_query::Expr::value(existing_verified_at.unwrap_or_else(Utc::now)),
            )
            .col_expr(
                sites::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(Some(Utc::now())),
            )
            .filter(sites::Column::Id.eq(site_id))
            .exec(&transaction)
            .await?;

        transaction.commit().await?;
        Ok(())
    }

    async fn complete_verification(
        &self,
        site_id: &str,
        origin: &Origin,
        token_id: i64,
    ) -> Result<bool> {
        let transaction = self.db.begin().await?;

        let consumed = verification_tokens::Entity::update_many()
            .col_expr(
                verification_tokens::Column::ConsumedAt,
                sea_orm::sea_query::Expr::value(Utc::now()),
            )
            .filter(verification_tokens::Column::Id.eq(token_id))
            .filter(verification_tokens::Column::SiteId.eq(site_id))
            .filter(verification_tokens::Column::ConsumedAt.is_null())
            .exec(&transaction)
            .await?;
        let first_consumption = consumed.rows_affected == 1;

        site_verified_origins::Entity::insert(site_verified_origins::ActiveModel {
            site_id: Set(site_id.to_owned()),
            origin: Set(origin.as_str().to_owned()),
            created_at: Set(Utc::now()),
        })
        .on_conflict(
            sea_orm::sea_query::OnConflict::columns([
                site_verified_origins::Column::SiteId,
                site_verified_origins::Column::Origin,
            ])
            .update_column(site_verified_origins::Column::CreatedAt)
            .to_owned(),
        )
        .exec(&transaction)
        .await?;

        let existing_verified_at = sites::Entity::find_by_id(site_id.to_owned())
            .one(&transaction)
            .await?
            .and_then(|site| site.verified_at);
        sites::Entity::update_many()
            .col_expr(
                sites::Column::VerificationStatus,
                sea_orm::sea_query::Expr::value(DbVerificationStatus::Verified),
            )
            .col_expr(
                sites::Column::VerifiedAt,
                sea_orm::sea_query::Expr::value(existing_verified_at.unwrap_or_else(Utc::now)),
            )
            .col_expr(
                sites::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(Some(Utc::now())),
            )
            .filter(sites::Column::Id.eq(site_id))
            .exec(&transaction)
            .await?;

        transaction.commit().await?;
        Ok(first_consumption)
    }

    async fn store_site_secret(&self, site_id: &str, secret: &str) -> Result<()> {
        sites::Entity::update_many()
            .col_expr(
                sites::Column::Secret,
                sea_orm::sea_query::Expr::value(Some(secret.to_owned())),
            )
            .col_expr(
                sites::Column::AuthMode,
                sea_orm::sea_query::Expr::value(DbAuthMode::Secret),
            )
            .col_expr(
                sites::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(Some(Utc::now())),
            )
            .filter(sites::Column::Id.eq(site_id))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn list_site_auth(&self) -> Result<Vec<SiteAuthInfo>> {
        let sites = sites::Entity::find().all(&self.db).await?;
        let origin_rows = site_verified_origins::Entity::find().all(&self.db).await?;
        let mut origins_by_site: HashMap<String, Vec<String>> = HashMap::new();
        for row in origin_rows {
            origins_by_site
                .entry(row.site_id)
                .or_default()
                .push(row.origin);
        }

        sites
            .into_iter()
            .map(|site| {
                let verified_origins = origins_by_site
                    .remove(&site.id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|origin| Origin::parse(&origin))
                    .collect::<Result<Vec<_>>>()?;
                Ok(SiteAuthInfo {
                    site_id: site.id,
                    lifecycle: core_lifecycle(site.lifecycle_status),
                    is_custom_id: site.is_custom_id,
                    auth_mode: core_auth_mode(site.auth_mode),
                    verification_status: core_verification_status(site.verification_status),
                    verified_origins,
                    verified_at: site.verified_at,
                    secret: site.secret,
                    claim_token_hash: site.claim_token_hash,
                    updated_at: site.updated_at,
                })
            })
            .collect()
    }

    async fn revoke_verified_origin(&self, site_id: &str, origin: &Origin) -> Result<bool> {
        let transaction = self.db.begin().await?;
        let deleted = site_verified_origins::Entity::delete_many()
            .filter(site_verified_origins::Column::SiteId.eq(site_id))
            .filter(site_verified_origins::Column::Origin.eq(origin.as_str()))
            .exec(&transaction)
            .await?;
        if deleted.rows_affected == 0 {
            transaction.commit().await?;
            return Ok(false);
        }

        let remaining = site_verified_origins::Entity::find()
            .filter(site_verified_origins::Column::SiteId.eq(site_id))
            .all(&transaction)
            .await?;
        if remaining.is_empty() {
            sites::Entity::update_many()
                .col_expr(
                    sites::Column::VerificationStatus,
                    sea_orm::sea_query::Expr::value(DbVerificationStatus::Unverified),
                )
                .col_expr(
                    sites::Column::VerifiedAt,
                    sea_orm::sea_query::Expr::value(None::<chrono::DateTime<chrono::Utc>>),
                )
                .col_expr(
                    sites::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(Some(Utc::now())),
                )
                .filter(sites::Column::Id.eq(site_id))
                .exec(&transaction)
                .await?;
        }

        transaction.commit().await?;
        Ok(true)
    }

    async fn clear_site_secret(&self, site_id: &str) -> Result<bool> {
        let result = sites::Entity::update_many()
            .col_expr(
                sites::Column::Secret,
                sea_orm::sea_query::Expr::value(None::<String>),
            )
            .col_expr(
                sites::Column::AuthMode,
                sea_orm::sea_query::Expr::value(DbAuthMode::Origin),
            )
            .col_expr(
                sites::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(Some(Utc::now())),
            )
            .filter(sites::Column::Id.eq(site_id))
            .exec(&self.db)
            .await?;
        Ok(result.rows_affected == 1)
    }
}

fn core_lifecycle(status: DbLifecycle) -> SiteLifecycle {
    match status {
        DbLifecycle::Active => SiteLifecycle::Active,
        DbLifecycle::Retiring => SiteLifecycle::Retiring,
        DbLifecycle::Retired => SiteLifecycle::Retired,
    }
}

fn lifecycle_value(status: DbLifecycle) -> &'static str {
    match status {
        DbLifecycle::Active => "active",
        DbLifecycle::Retiring => "retiring",
        DbLifecycle::Retired => "retired",
    }
}

fn core_auth_mode(mode: DbAuthMode) -> SiteAuthMode {
    match mode {
        DbAuthMode::Origin => SiteAuthMode::Origin,
        DbAuthMode::Secret => SiteAuthMode::Secret,
    }
}

fn core_verification_status(status: DbVerificationStatus) -> SiteVerificationStatus {
    match status {
        DbVerificationStatus::Unverified => SiteVerificationStatus::Unverified,
        DbVerificationStatus::Verified => SiteVerificationStatus::Verified,
    }
}

fn core_verification_token(row: verification_tokens::Model) -> Result<VerificationToken> {
    Ok(VerificationToken {
        id: row.id,
        site_id: row.site_id,
        origin: Origin::parse(&row.origin)?,
        token_hash: row.token_hash,
        methods: serde_json::from_str(&row.methods)
            .map_err(|e| anyhow::anyhow!("invalid verification methods: {e}"))?,
        expires_at: row.expires_at,
        consumed_at: row.consumed_at,
        created_at: row.created_at,
        attempts: row.attempts as u32,
    })
}
