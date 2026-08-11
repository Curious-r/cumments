use sea_orm_migration::prelude::*;

/// Rename `virtual_users.fingerprint` to `public_key`: the column stores the
/// full base64url Ed25519 public key, not a fingerprint.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Entity-first schemas already create the new column on fresh
        // databases; only existing installations need the rename.
        if super::column_exists(manager, "virtual_users", "fingerprint").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("virtual_users"))
                        .rename_column(Alias::new("fingerprint"), Alias::new("public_key"))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if super::column_exists(manager, "virtual_users", "public_key").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("virtual_users"))
                        .rename_column(Alias::new("public_key"), Alias::new("fingerprint"))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}
