//! The `MatrixDriver` contract for the AppService driver.
//!
//! A trait can only be implemented once for a type, so the public contract
//! lives here in one place and delegates to the domain-specific `*_impl`
//! methods (membership, rooms, versions, messages).

use super::*;
use anyhow::Result;
use async_trait::async_trait;
use cumments_core::{
    models::{CommentMedia, PostSlug, RoomEventPage, SiteId},
    ports::MatrixDriver,
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
        post_slug: Option<&PostSlug>,
    ) -> Result<()> {
        self.remove_room_alias_impl(site_id, post_slug).await
    }

    async fn delete_media(&self, server: &str, media_id: &str) -> Result<bool> {
        self.delete_media_impl(server, media_id).await
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

    async fn ensure_comment_room(
        &self,
        site_id: &SiteId,
        post_slug: &PostSlug,
        space_id: &str,
        candidate_room_id: Option<&str>,
    ) -> Result<String> {
        self.ensure_comment_room_impl(site_id, post_slug, space_id, candidate_room_id)
            .await
    }

    #[allow(clippy::too_many_arguments)] // driver methods carry the full event payload
    async fn post_message(
        &self,
        room_id: &str,
        content: &str,
        media: Option<&CommentMedia>,
        display_name: &str,
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
        site_id: &SiteId,
        reply_to: Option<&str>,
        reply_to_body: Option<&str>,
        reply_to_sender: Option<&str>,
        submission_id: Option<i64>,
        txn_id: &str,
    ) -> Result<String> {
        self.post_message_impl(
            room_id,
            content,
            media,
            display_name,
            author_public_key,
            author_signature,
            author_challenge,
            site_id,
            reply_to,
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
    ) -> Result<()> {
        self.react_message_impl(
            room_id,
            target_event_id,
            key,
            site_id,
            author_public_key,
            author_signature,
            author_challenge,
        )
        .await
    }

    async fn vote_poll(
        &self,
        room_id: &str,
        poll_event_id: &str,
        answer_id: &str,
        site_id: &SiteId,
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
    ) -> Result<()> {
        self.vote_poll_impl(
            room_id,
            poll_event_id,
            answer_id,
            site_id,
            author_public_key,
            author_signature,
            author_challenge,
        )
        .await
    }

    async fn post_location(
        &self,
        room_id: &str,
        geo_uri: &str,
        description: Option<&str>,
        display_name: &str,
        site_id: &SiteId,
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
        submission_id: Option<i64>,
        txn_id: &str,
    ) -> Result<String> {
        self.post_location_impl(
            room_id,
            geo_uri,
            description,
            display_name,
            site_id,
            author_public_key,
            author_signature,
            author_challenge,
            submission_id,
            txn_id,
        )
        .await
    }

    async fn update_message(
        &self,
        room_id: &str,
        event_id: &str,
        new_content: &str,
        display_name: &str,
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
            display_name,
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

    async fn get_room_metadata(&self, room_id: &str) -> Result<Option<serde_json::Value>> {
        self.get_room_metadata_impl(room_id).await
    }

    async fn get_room_canonical_alias(&self, room_id: &str) -> Result<Option<String>> {
        self.get_room_canonical_alias_impl(room_id).await
    }

    async fn event_exists(&self, room_id: &str, event_id: &str) -> Result<bool> {
        self.event_exists_impl(room_id, event_id).await
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

    async fn invite_user(&self, room_id: &str, user_id: &str) -> Result<()> {
        self.invite_user_impl(room_id, user_id).await
    }
}
