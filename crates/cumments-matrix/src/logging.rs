use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use cumments_core::{
    identity::derive_visitor_id_from_public_key,
    models::{CommentMedia, PageSlug, RoomEventPage, SiteId, VisitorProfile},
    ports::MatrixDriver,
};
use tracing::{debug, info};

pub struct LoggingMatrixDriver;

#[async_trait]
impl MatrixDriver for LoggingMatrixDriver {
    async fn ensure_comment_room(
        &self,
        site_id: &SiteId,
        page_slug: &PageSlug,
        _space_id: &str,
        _candidate_room_id: Option<&str>,
    ) -> Result<String> {
        info!(
            "LOGGING: Ensure comment room for site={} page={}",
            site_id.as_str(),
            page_slug.as_str()
        );
        Ok(format!(
            "log_room_{}_{}",
            site_id.as_str(),
            page_slug.as_str()
        ))
    }

    async fn create_site_space(&self, site_id: &SiteId) -> Result<String> {
        info!("LOGGING: Create space for site={}", site_id.as_str());
        Ok(format!("log_space_{}", site_id.as_str()))
    }

    async fn set_room_name(&self, room_id: &str, name: &str) -> Result<()> {
        info!("LOGGING: Set name of {room_id} to {name:?} (no-op)");
        Ok(())
    }

    async fn leave_room(&self, room_id: &str) -> Result<()> {
        info!("LOGGING: Leave room {room_id} (no-op)");
        Ok(())
    }

    async fn leave_room_as(&self, room_id: &str, user_id: &str) -> Result<()> {
        info!("LOGGING: Leave room {room_id} as {user_id} (no-op)");
        Ok(())
    }

    async fn join_room(&self, room_id: &str) -> Result<()> {
        info!("LOGGING: Join room {room_id} (no-op)");
        Ok(())
    }

    async fn remove_room_alias(
        &self,
        site_id: &SiteId,
        page_slug: Option<&PageSlug>,
    ) -> Result<()> {
        info!(
            "LOGGING: Remove alias for site={} page={:?} (no-op)",
            site_id.as_str(),
            page_slug.map(|slug| slug.as_str())
        );
        Ok(())
    }

    async fn delete_media(&self, server: &str, media_id: &str) -> Result<bool> {
        info!("LOGGING: Delete media {server}/{media_id} (no-op)");
        Ok(true)
    }

    async fn upload_media(
        &self,
        bytes: Bytes,
        filename: &str,
        mimetype: &str,
        author_public_key: &str,
        site_id: &SiteId,
    ) -> Result<String> {
        info!(
            "LOGGING: Upload media {filename} ({mimetype}, {} bytes) as \
             {author_public_key} for site={} (no-op)",
            bytes.len(),
            site_id.as_str()
        );
        Ok(format!("mxc://logging/{}/{}", site_id.as_str(), filename))
    }

    async fn set_avatar_url(
        &self,
        author_public_key: &str,
        site_id: &SiteId,
        avatar_url: Option<&str>,
    ) -> Result<()> {
        info!(
            "LOGGING: Set avatar for {author_public_key} on site={} to {avatar_url:?} (no-op)",
            site_id.as_str()
        );
        Ok(())
    }

    async fn get_profile(
        &self,
        author_public_key: &str,
        site_id: &SiteId,
    ) -> Result<Option<VisitorProfile>> {
        info!(
            "LOGGING: Get profile for {author_public_key} on site={} (no-op)",
            site_id.as_str()
        );
        Ok(None)
    }

    #[allow(clippy::too_many_arguments)]
    async fn post_message(
        &self,
        room_id: &str,
        content: &str,
        media: Option<&CommentMedia>,
        display_name: &str,
        author_public_key: &str,
        _author_signature: &str,
        _author_challenge: &str,
        _site_id: &SiteId,
        reply_to: Option<&str>,
        _thread_root: Option<&str>,
        reply_to_body: Option<&str>,
        reply_to_sender: Option<&str>,
        submission_id: Option<i64>,
        _txn_id: &str,
    ) -> Result<String> {
        let visitor_id = derive_visitor_id_from_public_key(author_public_key)
            .unwrap_or_else(|| "invalid".to_string());
        debug!(
            "LOGGING: Post message to room={}. Author={} (visitor={}, reply_to={:?}, reply_to_body={:?}, reply_to_sender={:?}, submission={:?}): {}",
            room_id,
            display_name,
            visitor_id,
            reply_to,
            reply_to_body,
            reply_to_sender,
            submission_id,
            media.as_ref().map(|m| m.url.as_str()).unwrap_or(content)
        );
        Ok(format!(
            "log_event_{}_{}",
            submission_id.unwrap_or(0),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ))
    }

    async fn react_message(
        &self,
        room_id: &str,
        target_event_id: &str,
        key: &str,
        _site_id: &SiteId,
        _author_public_key: &str,
        _author_signature: &str,
        _author_challenge: &str,
    ) -> Result<()> {
        info!("LOGGING: React to {target_event_id} in {room_id} with {key}");
        Ok(())
    }

    async fn vote_poll(
        &self,
        room_id: &str,
        poll_event_id: &str,
        answer_id: &str,
        _site_id: &SiteId,
        _author_public_key: &str,
        _author_signature: &str,
        _author_challenge: &str,
    ) -> Result<()> {
        info!("LOGGING: Vote on poll {poll_event_id} in {room_id} with {answer_id}");
        Ok(())
    }

    async fn post_location(
        &self,
        room_id: &str,
        geo_uri: &str,
        description: Option<&str>,
        display_name: &str,
        _site_id: &SiteId,
        _author_public_key: &str,
        _author_signature: &str,
        _author_challenge: &str,
        submission_id: Option<i64>,
        _reply_to: Option<&str>,
        _thread_root: Option<&str>,
        _txn_id: &str,
    ) -> Result<String> {
        info!(
            "LOGGING: Post location {geo_uri} in {room_id} ({}) as {} submission {}",
            description.unwrap_or(""),
            display_name,
            submission_id.map_or_else(|| "-".to_string(), |id| id.to_string())
        );
        Ok(format!("$logging-location-{geo_uri}"))
    }

    async fn update_message(
        &self,
        room_id: &str,
        event_id: &str,
        new_content: &str,
        display_name: &str,
        author_public_key: &str,
        _author_signature: &str,
        _author_challenge: &str,
        _site_id: &SiteId,
        submission_id: Option<i64>,
        _txn_id: &str,
    ) -> Result<String> {
        let visitor_id = derive_visitor_id_from_public_key(author_public_key)
            .unwrap_or_else(|| "invalid".to_string());
        debug!(
            "LOGGING: Update message {} in room={}. Author={} (visitor={}, submission={:?}): {}",
            event_id, room_id, display_name, visitor_id, submission_id, new_content
        );
        Ok(format!("log_update_{}", event_id))
    }

    async fn redact_message(
        &self,
        room_id: &str,
        event_id: &str,
        submission_id: Option<i64>,
        proof: Option<&serde_json::Value>,
        _txn_id: &str,
    ) -> Result<String> {
        info!(
            "LOGGING: Redact message {} in room={} (submission={:?}, proof={})",
            event_id,
            room_id,
            submission_id,
            proof.is_some()
        );
        Ok(format!("log_redact_{}", event_id))
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

    async fn get_joined_rooms(&self) -> Result<Vec<String>> {
        info!("LOGGING: Joined rooms (no real homeserver)");
        Ok(Vec::new())
    }

    async fn get_joined_members(&self, _room_id: &str) -> Result<Vec<String>> {
        info!("LOGGING: Joined members (no real homeserver)");
        Ok(Vec::new())
    }

    async fn send_bot_message(&self, room_id: &str, body: &str) -> Result<String> {
        info!("LOGGING: Bot message to {room_id}: {body}");
        Ok("$logging:hs".to_string())
    }

    async fn get_room_metadata(&self, room_id: &str) -> Result<Option<serde_json::Value>> {
        info!(
            "LOGGING: Room metadata for {} (no real homeserver)",
            room_id
        );
        Ok(None)
    }

    async fn get_room_canonical_alias(&self, room_id: &str) -> Result<Option<String>> {
        info!(
            "LOGGING: Canonical alias for {} (no real homeserver)",
            room_id
        );
        Ok(None)
    }

    fn sender_user_id(&self) -> Option<String> {
        None
    }

    async fn get_room_power_levels(&self, room_id: &str) -> Result<Option<serde_json::Value>> {
        info!(
            "LOGGING: Read power levels for {} (no real homeserver)",
            room_id
        );
        Ok(None)
    }

    async fn set_room_power_levels(
        &self,
        room_id: &str,
        content: &serde_json::Value,
    ) -> Result<()> {
        info!(
            "LOGGING: Set power levels for {}: {}",
            room_id,
            serde_json::to_string(content).unwrap_or_default()
        );
        Ok(())
    }

    async fn get_room_state(
        &self,
        room_id: &str,
        event_type: &str,
        state_key: &str,
    ) -> Result<Option<serde_json::Value>> {
        info!("LOGGING: Read state {event_type}/{state_key} for {room_id} (no-op)");
        Ok(None)
    }

    async fn set_room_state(
        &self,
        room_id: &str,
        event_type: &str,
        state_key: &str,
        content: &serde_json::Value,
    ) -> Result<String> {
        info!(
            "LOGGING: Set state {event_type}/{state_key} for {room_id}: {}",
            serde_json::to_string(content).unwrap_or_default()
        );
        Ok("$logging:state".to_string())
    }

    async fn upgrade_room(&self, room_id: &str, new_version: &str) -> Result<String> {
        info!("LOGGING: Upgrade {room_id} to {new_version} (no real homeserver)");
        Ok("!logging-upgraded:hs".to_string())
    }

    async fn adopt_room(
        &self,
        room_id: &str,
        site_id: &SiteId,
        page_slug: Option<&PageSlug>,
        _require_space: bool,
    ) -> Result<()> {
        info!(
            "LOGGING: Adopt {room_id} for {} ({}) (no real homeserver)",
            site_id.as_str(),
            page_slug.as_ref().map(|s| s.as_str()).unwrap_or("-")
        );
        Ok(())
    }

    async fn link_room_to_space(&self, space_id: &str, room_id: &str) -> Result<()> {
        info!("LOGGING: Link {room_id} under {space_id} (no-op)");
        Ok(())
    }

    async fn invite_user(&self, room_id: &str, user_id: &str) -> Result<()> {
        info!("LOGGING: Invite {user_id} to {room_id} (no-op)");
        Ok(())
    }
}
