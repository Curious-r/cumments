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

    pub async fn get_backfill_state(
        &self,
        room_id: &str,
    ) -> anyhow::Result<(Option<String>, Option<chrono::NaiveDateTime>)> {
        let row = sqlx::query!(
            "SELECT backfill_token, last_backfilled_at FROM rooms WHERE room_id = ?",
            room_id
        )
        .fetch_optional(&self.pool)
        .await?;

        if let Some(r) = row {
            Ok((r.backfill_token, r.last_backfilled_at))
        } else {
            Ok((None, None))
        }
    }

    pub async fn update_backfill_state(
        &self,
        room_id: &str,
        token: Option<&str>,
    ) -> anyhow::Result<()> {
        let now = chrono::Utc::now().naive_utc();
        if let Some(t) = token {
            sqlx::query(
                "UPDATE rooms SET backfill_token = ?, last_backfilled_at = ? WHERE room_id = ?",
            )
            .bind(t)
            .bind(now)
            .bind(room_id)
            .execute(&self.pool)
            .await?;
        } else {
            sqlx::query("UPDATE rooms SET last_backfilled_at = ? WHERE room_id = ?")
                .bind(now)
                .bind(room_id)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }
}
