use crate::Db;
use sqlx::Row;
impl Db {
    pub async fn get_sync_token(&self) -> anyhow::Result<Option<String>> {
        let row = sqlx::query("SELECT value FROM meta WHERE key = 'sync_token'")
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get(0)))
    }
    pub async fn save_sync_token(&self, token: &str) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO meta (key, value) VALUES ('sync_token', ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value"
        )
        .bind(token)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
