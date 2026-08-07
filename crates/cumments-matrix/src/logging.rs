use anyhow::Result;
use async_trait::async_trait;
use cumments_core::{
    identity::derive_visitor_id_from_public_key,
    models::{PostSlug, RoomEventPage, SiteId},
    ports::MatrixDriver,
};
use tracing::info;

pub struct LoggingMatrixDriver;

#[async_trait]
impl MatrixDriver for LoggingMatrixDriver {
    async fn ensure_comment_room(
        &self,
        site_id: &SiteId,
        post_slug: &PostSlug,
        _space_id: &str,
        _candidate_room_id: Option<&str>,
    ) -> Result<String> {
        info!(
            "LOGGING: Ensure comment room for site={} post={}",
            site_id.as_str(),
            post_slug.as_str()
        );
        Ok(format!(
            "log_room_{}_{}",
            site_id.as_str(),
            post_slug.as_str()
        ))
    }

    async fn create_site_space(&self, site_id: &SiteId) -> Result<String> {
        info!("LOGGING: Create space for site={}", site_id.as_str());
        Ok(format!("log_space_{}", site_id.as_str()))
    }

    #[allow(clippy::too_many_arguments)]
    async fn post_message(
        &self,
        room_id: &str,
        content: &str,
        nickname: &str,
        author_public_key: &str,
        _author_signature: &str,
        _site_id: &SiteId,
        intent_id: Option<i64>,
    ) -> Result<String> {
        let visitor_id = derive_visitor_id_from_public_key(author_public_key)
            .unwrap_or_else(|| "invalid".to_string());
        info!(
            "LOGGING: Post message to room={}. Author={} (visitor={}, intent={:?}): {}",
            room_id, nickname, visitor_id, intent_id, content
        );
        Ok("log_event_id".to_string())
    }

    async fn update_message(
        &self,
        room_id: &str,
        event_id: &str,
        new_content: &str,
        nickname: &str,
        author_public_key: &str,
        _author_signature: &str,
        _site_id: &SiteId,
    ) -> Result<String> {
        let visitor_id = derive_visitor_id_from_public_key(author_public_key)
            .unwrap_or_else(|| "invalid".to_string());
        info!(
            "LOGGING: Update message {} in room={}. Author={} (visitor={}): {}",
            event_id, room_id, nickname, visitor_id, new_content
        );
        Ok(format!("log_update_{}", event_id))
    }

    async fn redact_message(&self, room_id: &str, event_id: &str) -> Result<()> {
        info!("LOGGING: Redact message {} in room={}", event_id, room_id);
        Ok(())
    }

    async fn event_exists(&self, _room_id: &str, event_id: &str) -> Result<bool> {
        info!("LOGGING: Event exists? {} (no real homeserver)", event_id);
        // No real homeserver: the projector can never close the loop, so treat
        // timed-out events as absent and let the retry budget drain visibly.
        Ok(false)
    }

    async fn get_room_events(
        &self,
        room_id: &str,
        _from: Option<&str>,
        _limit: u32,
    ) -> Result<RoomEventPage> {
        info!(
            "LOGGING: Fetch room events for {} (no real homeserver)",
            room_id
        );
        Ok(RoomEventPage::default())
    }

    async fn joined_rooms(&self) -> Result<Vec<String>> {
        info!("LOGGING: Joined rooms (no real homeserver)");
        Ok(Vec::new())
    }

    async fn room_metadata(&self, room_id: &str) -> Result<Option<serde_json::Value>> {
        info!(
            "LOGGING: Room metadata for {} (no real homeserver)",
            room_id
        );
        Ok(None)
    }
}
