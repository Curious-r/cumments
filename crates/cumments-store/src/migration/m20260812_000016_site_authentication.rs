use sea_orm_migration::prelude::*;

use crate::entities::*;
use crate::migration::column_exists;

const SITES_TABLE: &str = "sites";
const AUTH_MODE: &str = "auth_mode";
const VERIFICATION_STATUS: &str = "verification_status";
const CLAIM_TOKEN_HASH: &str = "claim_token_hash";
const SECRET: &str = "secret";
const VERIFIED_AT: &str = "verified_at";
const UPDATED_AT: &str = "updated_at";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        add_sites_column(
            manager,
            AUTH_MODE,
            ColumnDef::new(Alias::new(AUTH_MODE))
                .string()
                .not_null()
                .default("origin")
                .into(),
        )
        .await?;
        add_sites_column(
            manager,
            VERIFICATION_STATUS,
            ColumnDef::new(Alias::new(VERIFICATION_STATUS))
                .string()
                .not_null()
                .default("unverified")
                .into(),
        )
        .await?;
        add_sites_column(
            manager,
            CLAIM_TOKEN_HASH,
            ColumnDef::new(Alias::new(CLAIM_TOKEN_HASH))
                .string()
                .null()
                .into(),
        )
        .await?;
        add_sites_column(
            manager,
            SECRET,
            ColumnDef::new(Alias::new(SECRET)).string().null().into(),
        )
        .await?;
        add_sites_column(
            manager,
            VERIFIED_AT,
            ColumnDef::new(Alias::new(VERIFIED_AT))
                .date_time()
                .null()
                .into(),
        )
        .await?;
        add_sites_column(
            manager,
            UPDATED_AT,
            ColumnDef::new(Alias::new(UPDATED_AT))
                .date_time()
                .null()
                .into(),
        )
        .await?;

        let builder = manager.get_database_backend();
        let schema = sea_orm::Schema::new(builder);
        manager
            .create_table(schema.create_table_from_entity(site_verified_origins::Entity))
            .await?;
        manager
            .create_table(schema.create_table_from_entity(verification_tokens::Entity))
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(verification_tokens::Entity).to_owned())
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(site_verified_origins::Entity)
                    .to_owned(),
            )
            .await?;

        for column in [
            AUTH_MODE,
            VERIFICATION_STATUS,
            CLAIM_TOKEN_HASH,
            SECRET,
            VERIFIED_AT,
            UPDATED_AT,
        ] {
            if column_exists(manager, SITES_TABLE, column).await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(sites::Entity)
                            .drop_column(Alias::new(column))
                            .to_owned(),
                    )
                    .await?;
            }
        }

        Ok(())
    }
}

/// Adds a column to `sites` when it is missing (entity-first migrations create
/// the column on fresh databases).
async fn add_sites_column(
    manager: &SchemaManager<'_>,
    column: &str,
    definition: ColumnDef,
) -> Result<(), DbErr> {
    if column_exists(manager, SITES_TABLE, column).await? {
        return Ok(());
    }
    manager
        .alter_table(
            Table::alter()
                .table(sites::Entity)
                .add_column(definition)
                .to_owned(),
        )
        .await
}
