use anyhow::Result;
use async_trait::async_trait;
use cumments_core::{
    models::{PostSlug, SiteId},
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

    async fn post_message(
        &self,
        room_id: &str,
        content: &str,
        nickname: &str,
        fingerprint: &str,
    ) -> Result<String> {
        info!(
            "LOGGING: Post message to room={}. Author={} (fp={}): {}",
            room_id, nickname, fingerprint, content
        );
        Ok("log_event_id".to_string())
    }

    async fn update_message(
        &self,
        room_id: &str,
        event_id: &str,
        new_content: &str,
        nickname: &str,
        fingerprint: &str,
    ) -> Result<String> {
        info!(
            "LOGGING: Update message {} in room={}. Author={} (fp={}): {}",
            event_id, room_id, nickname, fingerprint, new_content
        );
        Ok(format!("log_update_{}", event_id))
    }

    async fn redact_message(&self, room_id: &str, event_id: &str) -> Result<()> {
        info!("LOGGING: Redact message {} in room={}", event_id, room_id);
        Ok(())
    }
}
