use crate::{models::SqlProfile, Db};
use chrono::Utc;

impl Db {
    // 获取本地缓存的 Profile
    pub async fn get_cached_profile(&self, user_id: &str) -> anyhow::Result<Option<SqlProfile>> {
        let threshold = Utc::now().naive_utc() - chrono::Duration::hours(24);

        let profile = sqlx::query_as!(
            SqlProfile,
            r#"
            SELECT
                user_id as "user_id!",
                display_name,
                avatar_url,
                last_updated_at
            FROM profiles
            WHERE user_id = ? AND last_updated_at > ?
            "#,
            user_id,
            threshold
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(profile)
    }

    // 更新 Profile 缓存
    pub async fn upsert_profile(
        &self,
        user_id: &str,
        display_name: Option<&str>,
        avatar_url: Option<&str>,
    ) -> anyhow::Result<()> {
        let now = Utc::now().naive_utc();

        sqlx::query(
            r#"
            INSERT INTO profiles (user_id, display_name, avatar_url, last_updated_at)
            VALUES (?, ?, ?, ?)
            ON CONFLICT(user_id) DO UPDATE SET
                display_name = excluded.display_name,
                avatar_url = excluded.avatar_url,
                last_updated_at = excluded.last_updated_at
            "#,
        )
        .bind(user_id)
        .bind(display_name)
        .bind(avatar_url)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}
