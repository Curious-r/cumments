use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Distinguish guest comments (AS virtual users, Ed25519 ownership) from
/// Matrix-native comments (regular Matrix accounts, room-power ownership).
///
/// Fresh databases already get the column from the current entity via
/// migration 000001, so the column addition must be idempotent. Existing rows
/// are backfilled from the legacy signal: a stored public key means guest,
/// otherwise the row is a Matrix-native comment.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !column_exists(manager, "comments", "author_type").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("comments"))
                        .add_column(
                            ColumnDef::new(Alias::new("author_type"))
                                .string()
                                .not_null()
                                .default("guest"),
                        )
                        .to_owned(),
                )
                .await?;
        }

        // Backfill legacy rows (idempotent: guarded by IS NULL).
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE comments SET author_type = 'guest' \
                 WHERE author_type IS NULL AND author_public_key IS NOT NULL",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE comments SET author_type = 'matrix' WHERE author_type IS NULL",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if column_exists(manager, "comments", "author_type").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("comments"))
                        .drop_column(Alias::new("author_type"))
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
}
