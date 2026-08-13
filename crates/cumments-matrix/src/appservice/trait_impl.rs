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
        intent_id: Option<i64>,
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
            intent_id,
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
        site_id: &SiteId,
        author_public_key: &str,
        author_signature: &str,
        author_challenge: &str,
    ) -> Result<()> {
        self.post_location_impl(
            room_id,
            geo_uri,
            description,
            site_id,
            author_public_key,
            author_signature,
            author_challenge,
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
        intent_id: Option<i64>,
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
            intent_id,
        )
        .await
    }

    async fn redact_message(
        &self,
        room_id: &str,
        event_id: &str,
        intent_id: Option<i64>,
        proof: Option<&serde_json::Value>,
    ) -> Result<()> {
        self.redact_message_impl(room_id, event_id, intent_id, proof)
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

    async fn ensure_admin(&self, room_id: &str) {
        self.ensure_admin_impl(room_id).await;
    }
}
