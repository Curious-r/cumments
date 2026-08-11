use super::DbStore;
use crate::entities::active_enums::{
    SiteAuthMode as DbAuthMode, SiteVerificationStatus as DbVerificationStatus,
};
use crate::entities::{site_verified_origins, sites, verification_tokens};
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use cumments_core::ports::SiteAuthStore;
use cumments_core::site_auth::{
    NewVerificationToken, Origin, SiteAuthInfo, SiteAuthMode, SiteVerificationStatus,
    VerificationToken,
};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set, TransactionTrait};

#[async_trait]
impl SiteAuthStore for DbStore {
    async fn register_site(&self, site_id: &str, claim_token_hash: &str) -> Result<()> {
        let now = Utc::now();
        let model = sites::ActiveModel {
            id: Set(site_id.to_owned()),
            matrix_space_id: Set(String::new()),
            display_name: Set(None),
            auth_mode: Set(DbAuthMode::Origin),
            verification_status: Set(DbVerificationStatus::Unverified),
            claim_token_hash: Set(Some(claim_token_hash.to_owned())),
            secret: Set(None),
            verified_at: Set(None),
            created_at: Set(now),
            updated_at: Set(Some(now)),
        };

        if sites::Entity::find_by_id(site_id.to_owned())
            .one(&self.db)
            .await?
            .is_some()
        {
            anyhow::bail!("site id `{site_id}` already exists");
        }
        sites::Entity::insert(model).exec(&self.db).await?;
        Ok(())
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
            auth_mode: core_auth_mode(site.auth_mode),
            verification_status: core_verification_status(site.verification_status),
            verified_origins: origins,
            verified_at: site.verified_at,
            secret: site.secret,
        }))
    }

    async fn get_claim_token_hash(&self, site_id: &str) -> Result<Option<String>> {
        Ok(sites::Entity::find_by_id(site_id.to_owned())
            .one(&self.db)
            .await?
            .and_then(|site| site.claim_token_hash))
    }

    async fn insert_verification_tokens(&self, tokens: &[NewVerificationToken]) -> Result<()> {
        let now = Utc::now();
        let models = tokens
            .iter()
            .map(|token| verification_tokens::ActiveModel {
                id: Set(0),
                site_id: Set(token.site_id.clone()),
                origin: Set(token.origin.as_str().to_owned()),
                token_hash: Set(token.token_hash.clone()),
                methods: Set(serde_json::to_string(&token.methods).expect("methods serialize")),
                expires_at: Set(token.expires_at),
                consumed_at: Set(None),
                created_at: Set(now),
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
            .do_nothing()
            .to_owned(),
        )
        .exec(&transaction)
        .await?;

        sites::Entity::update_many()
            .col_expr(
                sites::Column::VerificationStatus,
                sea_orm::sea_query::Expr::value(DbVerificationStatus::Verified),
            )
            .col_expr(
                sites::Column::VerifiedAt,
                sea_orm::sea_query::Expr::value(Utc::now()),
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
        methods: serde_json::from_str(&row.methods).unwrap_or_default(),
        expires_at: row.expires_at,
        consumed_at: row.consumed_at,
        created_at: row.created_at,
    })
}
