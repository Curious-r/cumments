use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Redacted votes retain only their relation metadata; the selected choice is
/// authored content and is intentionally forgotten.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"UPDATE poll_response_events
                  SET option_index = NULL
                  WHERE redacted_at IS NOT NULL"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // The original vote choices were intentionally destroyed.
        Ok(())
    }
}
