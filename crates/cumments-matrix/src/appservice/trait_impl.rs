//! The `MatrixDriver` contract for the AppService driver.
//!
//! A trait can only be implemented once for a type, so the public contract
//! lives here in one place and delegates to the domain-specific `*_impl`
//! methods (membership, rooms, versions, messages).

use super::*;
use anyhow::Result;
use async_trait::async_trait;
use cumments_core::{
    models::{
        CommentMedia, MatrixEvent, MemberPresentation, PageSlug, RoomEventPage, SiteId,
        VisitorProfile,
    },
    ports::{HistoricalRoomStateResolver, MatrixDriver, MatrixProfileDriver},
    profile::ProfileDriverError,
};

#[async_trait]
impl MatrixDriver for AppServiceMatrixDriver {
    async fn create_site_space(&self, site_id: &SiteId) -> Result<String> {
        self.create_site_space_impl(site_id).await
    }

    async fn set_room_name(&self, room_id: &str, name: &str) -> Result<()> {
        self.set_room_name_impl(room_id, name).await
    }

    async fn leave_room(&self, room_id: &str) -> Result<()> {
        self.leave_room_impl(room_id).await
    }

    async fn leave_room_as(&self, room_id: &str, user_id: &str) -> Result<()> {
        self.leave_room_as_impl(room_id, user_id).await
    }

    async fn join_room(&self, room_id: &str) -> Result<()> {
        self.join_room_impl(room_id).await
    }

    async fn remove_room_alias(
        &self,
        site_id: &SiteId,
        page_slug: Option<&PageSlug>,
    ) -> Result<()> {
        self.remove_room_alias_impl(site_id, page_slug).await
    }

    async fn upload_media(
        &self,
        bytes: bytes::Bytes,
        filename: &str,
        mimetype: &str,
        author_public_key: &str,
        site_id: &SiteId,
    ) -> Result<String> {
        self.upload_media_impl(bytes, filename, mimetype, author_public_key, site_id)
            .await
    }

    async fn set_avatar_url(
        &self,
        author_public_key: &str,
        site_id: &SiteId,
        avatar_url: Option<&str>,
    ) -> Result<()> {
        self.set_avatar_url_impl(author_public_key, site_id, avatar_url)
            .await
    }

    async fn get_profile(
        &self,
        author_public_key: &str,
        site_id: &SiteId,
    ) -> Result<Option<VisitorProfile>> {
        self.get_profile_impl(author_public_key, site_id).await
    }

    async fn ensure_comment_room(
        &self,
        site_id: &SiteId,
        page_slug: &PageSlug,
        space_id: &str,
        candidate_room_id: Option<&str>,
    ) -> Result<String> {
        self.ensure_comment_room_impl(site_id, page_slug, space_id, candidate_room_id)
            .await
    }

    #[allow(clippy::too_many_arguments)] // driver methods carry the full event payload
    async fn post_message(
        &self,
        room_id: &str,
        content: &str,
        media: Option<&CommentMedia>,
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
        site_id: &SiteId,
        reply_to: Option<&str>,
        thread_root: Option<&str>,
        reply_to_body: Option<&str>,
        reply_to_sender: Option<&str>,
        submission_id: Option<i64>,
        txn_id: &str,
    ) -> Result<String> {
        self.post_message_impl(
            room_id,
            content,
            media,
            author_public_key,
            author_signature,
            author_challenge,
            site_id,
            reply_to,
            thread_root,
            reply_to_body,
            reply_to_sender,
            submission_id,
            txn_id,
        )
        .await
    }

    async fn react_message(
        &self,
        room_id: &str,
        target_event_id: &str,
        key: &str,
        site_id: &SiteId,
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
        txn_id: &str,
    ) -> Result<()> {
        self.react_message_impl(
            room_id,
            target_event_id,
            key,
            site_id,
            author_public_key,
            author_signature,
            author_challenge,
            txn_id,
        )
        .await
    }

    async fn post_poll_response(
        &self,
        request: cumments_core::ports::PollResponseRequest<'_>,
    ) -> Result<()> {
        self.post_poll_response_impl(request).await
    }

    async fn post_poll_end(&self, request: cumments_core::ports::PollEndRequest<'_>) -> Result<()> {
        self.post_poll_end_impl(request).await
    }

    async fn post_poll(
        &self,
        request: cumments_core::ports::PollStartRequest<'_>,
    ) -> Result<String> {
        self.post_poll_impl(request).await
    }

    async fn post_location(
        &self,
        room_id: &str,
        geo_uri: &str,
        description: Option<&str>,
        site_id: &SiteId,
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
        submission_id: Option<i64>,
        reply_to: Option<&str>,
        thread_root: Option<&str>,
        txn_id: &str,
    ) -> Result<String> {
        self.post_location_impl(
            room_id,
            geo_uri,
            description,
            site_id,
            author_public_key,
            author_signature,
            author_challenge,
            submission_id,
            reply_to,
            thread_root,
            txn_id,
        )
        .await
    }

    async fn update_message(
        &self,
        room_id: &str,
        event_id: &str,
        new_content: &str,
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
        site_id: &SiteId,
        submission_id: Option<i64>,
        txn_id: &str,
    ) -> Result<String> {
        self.update_message_impl(
            room_id,
            event_id,
            new_content,
            author_public_key,
            author_signature,
            author_challenge,
            site_id,
            submission_id,
            txn_id,
        )
        .await
    }

    async fn redact_message(
        &self,
        room_id: &str,
        event_id: &str,
        submission_id: Option<i64>,
        proof: Option<&serde_json::Value>,
        txn_id: &str,
    ) -> Result<String> {
        self.redact_message_impl(room_id, event_id, submission_id, proof, txn_id)
            .await
    }

    async fn get_room_events(
        &self,
        room_id: &str,
        from: Option<&str>,
        limit: u32,
    ) -> Result<RoomEventPage> {
        self.get_room_events_impl(room_id, from, limit).await
    }

    async fn get_joined_rooms(&self) -> Result<Vec<String>> {
        self.get_joined_rooms_impl().await
    }

    async fn get_joined_members(&self, room_id: &str) -> Result<Vec<String>> {
        self.get_joined_members_impl(room_id).await
    }

    async fn send_bot_message(&self, room_id: &str, body: &str) -> Result<String> {
        self.send_bot_message_impl(room_id, body).await
    }

    async fn get_room_metadata(&self, room_id: &str) -> Result<Option<serde_json::Value>> {
        self.get_room_metadata_impl(room_id).await
    }

    async fn get_room_canonical_alias(&self, room_id: &str) -> Result<Option<String>> {
        self.get_room_canonical_alias_impl(room_id).await
    }

    async fn event_exists(&self, room_id: &str, event_id: &str) -> Result<bool> {
        Ok(self.get_event_impl(room_id, event_id).await?.is_some())
    }

    async fn get_event(&self, room_id: &str, event_id: &str) -> Result<Option<MatrixEvent>> {
        self.get_event_impl(room_id, event_id).await
    }

    fn sender_user_id(&self) -> Option<String> {
        Some(self.sender_user_id())
    }

    async fn get_room_power_levels(&self, room_id: &str) -> Result<Option<serde_json::Value>> {
        self.get_power_levels(room_id).await
    }

    async fn set_room_power_levels(
        &self,
        room_id: &str,
        content: &serde_json::Value,
    ) -> Result<()> {
        self.write_power_levels(room_id, content).await
    }

    async fn get_room_state(
        &self,
        room_id: &str,
        event_type: &str,
        state_key: &str,
    ) -> Result<Option<serde_json::Value>> {
        self.read_state(room_id, event_type, state_key).await
    }

    async fn set_room_state(
        &self,
        room_id: &str,
        event_type: &str,
        state_key: &str,
        content: &serde_json::Value,
    ) -> Result<String> {
        self.write_state(room_id, event_type, state_key, content)
            .await
    }

    async fn upgrade_room(&self, room_id: &str, new_version: &str) -> Result<String> {
        self.upgrade_room_impl(room_id, new_version).await
    }

    async fn adopt_room(
        &self,
        room_id: &str,
        site_id: &SiteId,
        page_slug: Option<&PageSlug>,
        require_space: bool,
    ) -> Result<()> {
        self.adopt_room(room_id, site_id, page_slug, require_space)
            .await
    }

    async fn link_room_to_space(&self, space_id: &str, room_id: &str) -> Result<()> {
        self.link_room_to_space(space_id, room_id).await
    }

    async fn invite_user(&self, room_id: &str, user_id: &str) -> Result<()> {
        self.invite_user_impl(room_id, user_id).await
    }
}

#[async_trait]
impl MatrixProfileDriver for AppServiceMatrixDriver {
    async fn set_display_name(
        &self,
        author_public_key: &str,
        site_id: &SiteId,
        display_name: &str,
    ) -> std::result::Result<(), ProfileDriverError> {
        self.set_display_name_impl(author_public_key, site_id, display_name)
            .await
    }

    async fn clear_display_name(
        &self,
        author_public_key: &str,
        site_id: &SiteId,
    ) -> std::result::Result<(), ProfileDriverError> {
        self.clear_display_name_impl(author_public_key, site_id)
            .await
    }

    async fn set_avatar(
        &self,
        author_public_key: &str,
        site_id: &SiteId,
        avatar_url: &str,
    ) -> std::result::Result<(), ProfileDriverError> {
        self.set_avatar_impl(author_public_key, site_id, avatar_url)
            .await
    }

    async fn clear_avatar(
        &self,
        author_public_key: &str,
        site_id: &SiteId,
    ) -> std::result::Result<(), ProfileDriverError> {
        self.clear_avatar_impl(author_public_key, site_id).await
    }
}

#[async_trait]
impl HistoricalRoomStateResolver for AppServiceMatrixDriver {
    async fn resolve_member_presentation(
        &self,
        room_id: &str,
        event_id: &str,
        sender_mxid: &str,
    ) -> Result<Option<MemberPresentation>> {
        self.resolve_member_presentation_impl(room_id, event_id, sender_mxid)
            .await
    }
}
