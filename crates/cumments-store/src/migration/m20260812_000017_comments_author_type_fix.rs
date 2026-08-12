use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Correct rows poisoned by migration 000010.
///
/// 000010 added `author_type TEXT NOT NULL DEFAULT 'guest'`, which filled
/// every pre-existing row with `'guest'` and made its two
/// `WHERE author_type IS NULL` backfill statements dead code. A stored
/// public key is the reliable discriminator: guest comments always carry
/// one, Matrix-native comments never do.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE comments SET author_type = 'matrix' \
                 WHERE author_public_key IS NULL AND author_type = 'guest'",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE comments SET author_type = 'guest' \
                 WHERE author_public_key IS NOT NULL AND author_type = 'matrix'",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Not reversible: the corrected values are derived from data.
        Ok(())
    }
}
