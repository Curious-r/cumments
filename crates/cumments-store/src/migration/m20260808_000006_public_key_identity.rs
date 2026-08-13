use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Replace the local-only token verifier with a publicly verifiable Ed25519
/// public key, so ownership can be rebuilt from Matrix events.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // comments: token hash -> public key
        if column_exists(manager, "comments", "author_fingerprint").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("comments"))
                        .drop_column(Alias::new("author_fingerprint"))
                        .to_owned(),
                )
                .await?;
        }
        if column_exists(manager, "comments", "author_token_hash").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("comments"))
                        .drop_column(Alias::new("author_token_hash"))
                        .to_owned(),
                )
                .await?;
        }
        if !column_exists(manager, "comments", "author_public_key").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("comments"))
                        .add_column(
                            ColumnDef::new(Alias::new("author_public_key"))
                                .string()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }

        // post submission queue: token hash -> public key
        if column_exists(manager, "post_submissions", "author_token_hash").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("post_submissions"))
                        .drop_column(Alias::new("author_token_hash"))
                        .to_owned(),
                )
                .await?;
        }
        if !column_exists(manager, "post_submissions", "author_public_key").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("post_submissions"))
                        .add_column(
                            ColumnDef::new(Alias::new("author_public_key"))
                                .string()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }

        // update submission queue: fingerprint -> public key + signature
        if column_exists(manager, "update_submissions", "author_fingerprint").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("update_submissions"))
                        .drop_column(Alias::new("author_fingerprint"))
                        .to_owned(),
                )
                .await?;
        }
        if !column_exists(manager, "update_submissions", "author_public_key").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("update_submissions"))
                        .add_column(
                            ColumnDef::new(Alias::new("author_public_key"))
                                .string()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }
        if !column_exists(manager, "update_submissions", "author_signature").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("update_submissions"))
                        .add_column(
                            ColumnDef::new(Alias::new("author_signature"))
                                .string()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Recreate the old columns (best-effort rollback).
        if !column_exists(manager, "comments", "author_token_hash").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("comments"))
                        .add_column(
                            ColumnDef::new(Alias::new("author_token_hash"))
                                .string()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }
        if column_exists(manager, "comments", "author_public_key").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("comments"))
                        .drop_column(Alias::new("author_public_key"))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}
