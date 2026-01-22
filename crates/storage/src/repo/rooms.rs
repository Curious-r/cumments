use crate::Db;
use domain::SiteId;

impl Db {
    pub async fn ensure_room(
        &self,
        room_id: &str,
        site_id: &str,
        slug: &str,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO rooms (room_id, site_id, post_slug)
            VALUES (?, ?, ?)
            ON CONFLICT(room_id) DO NOTHING
            "#,
        )
        .bind(room_id)
        .bind(site_id)
        .bind(slug)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_room_meta(&self, room_id: &str) -> anyhow::Result<Option<(SiteId, String)>> {
        let row = sqlx::query!(
            "SELECT site_id, post_slug FROM rooms WHERE room_id = ?",
            room_id
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| (SiteId::new_unchecked(r.site_id), r.post_slug)))
    }
}
