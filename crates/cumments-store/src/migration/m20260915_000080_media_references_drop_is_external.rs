use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

const TABLE: &str = "media_references";
const COLUMN: &str = "is_external";

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Drop the upload-provenance flag from media reference mappings.
///
/// A `MediaReference` is the site-scoped presentation of a Matrix MXC and no
/// longer distinguishes how the media was uploaded. Existing rows and the
/// `(site_id, mxc_uri)` mapping are preserved unchanged.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if column_exists(manager, TABLE, COLUMN).await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new(TABLE))
                        .drop_column(Alias::new(COLUMN))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !column_exists(manager, TABLE, COLUMN).await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new(TABLE))
                        .add_column(
                            ColumnDef::new(Alias::new(COLUMN))
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}
