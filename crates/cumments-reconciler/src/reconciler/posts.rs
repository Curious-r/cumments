//! Post-command reconciliation: ensure the comment room, then send.

use super::*;
use anyhow::Result;
use async_trait::async_trait;
use tracing::{error, warn};

/// Reconciles pending post (and location) submissions toward Matrix.
pub struct PostsPass {
    deps: Arc<ReconcilerDeps>,
    config: PassConfig,
}

impl PostsPass {
    pub fn new(deps: Arc<ReconcilerDeps>, config: PassConfig) -> Self {
        Self { deps, config }
    }

    async fn reconcile(&self) -> Result<u64> {
        self.deps
            .submission_store
            .recover_expired_submission_leases()
            .await?;
        let lease_until = chrono::Utc::now() + SUBMISSION_LEASE;
        let submissions = self
            .deps
            .submission_store
            .claim_pending_post_submissions(SUBMISSION_BATCH_SIZE, lease_until)
            .await?;

        if submissions.is_empty() {
            return Ok(0);
        }
        let num_submissions = submissions.len() as u64;

        for pending in submissions {
            let id = pending.id;
            let command = pending.command;
            let process_result = run_submission(async {
                // 1. Brain: Ensure the site space is ready
                let space_id = self
                    .deps
                    .site_service
                    .ensure_space(&command.site_id, self.deps.driver.as_ref())
                    .await?;

                // 2. Registry: Check for existing room in local cache (O(1) hint)
                let candidate_room_id = self
                    .deps
                    .registry_store
                    .get_registered_room(&command.site_id, &command.post_slug)
                    .await?;

                // Quarantine gate: without an active candidate the driver
                // would retry the quarantined room through alias recovery on
                // every comment. Fail fast instead until its retry is due.
                if candidate_room_id.is_none()
                    && let Some(room) = quarantined_room_for(
                        &self.deps,
                        &command.site_id,
                        &command.post_slug,
                    )
                    .await?
                {
                    match room.next_attempt_at {
                        Some(next) if next > chrono::Utc::now() => {
                            return Err(anyhow::anyhow!(
                                "Room {} is quarantined until {}: {}",
                                room.room_id,
                                next.to_rfc3339(),
                                room.quarantine_reason
                            ));
                        }
                        None => {
                            return Err(anyhow::anyhow!(
                                "Room {} requires manual reinstatement: {}",
                                room.room_id,
                                room.quarantine_reason
                            ));
                        }
                        _ => {}
                    }
                }

                // 3. Hands: Ensure the post-specific room exists and is linked
                let room_id = match self
                    .deps
                    .driver
                    .ensure_comment_room(
                        &command.site_id,
                        &command.post_slug,
                        &space_id,
                        candidate_room_id.as_deref(),
                    )
                    .await
                {
                    Ok(room_id) => room_id,
                    Err(e) => {
                        if should_quarantine(&e) {
                            let room_id =
                                quarantine_target(&e).or_else(|| candidate_room_id.clone());
                            if let Some(room_id) = room_id {
                                let _ = record_adoption_failure(
                                    &self.deps,
                                    &room_id,
                                    &e.to_string(),
                                )
                                .await;
                            }
                        }
                        return Err(e);
                    }
                };

                // 3b. Registry: Write back the room mapping immediately
                // This is critical in AppService mode: pushes resolve room
                // identity from this registry, so the mapping must be durable
                // before any event can arrive.
                self.deps
                    .registry_store
                    .register_room(&room_id, &command.site_id, &command.post_slug)
                    .await?;

                // 3c. Resolve the replied-to comment from the read model so
                // the Matrix message can carry a rich-reply fallback quote.
                // Unknown originals (e.g. replies to a not-yet-projected
                // event) simply skip the quote.
                let (reply_to_body, reply_to_sender) = match command.reply_to.as_deref() {
                    Some(event_id) => {
                        match self.deps.message_store.get_message(event_id).await? {
                            Some(message) => {
                                let body = match &message.content {
                                    cumments_core::models::Content::Text(text) => {
                                        Some(text.body.clone())
                                    }
                                    _ => None,
                                };
                                (body, Some(message.sender_mxid))
                            }
                            None => (None, None),
                        }
                    }
                    None => (None, None),
                };

                // 4. Hands: Post the actual message
                let event_id = {
                    let result = if let Some(location) = &command.location {
                        self.deps
                            .driver
                            .post_location(
                                &room_id,
                                &location.geo_uri,
                                location.description.as_deref(),
                                &command.display_name,
                                &command.site_id,
                                &command.author_public_key,
                                &command.author_signature,
                                &command.author_challenge,
                                Some(id),
                                pending.force_new_txn,
                            )
                            .await
                    } else {
                        self.deps
                            .driver
                            .post_message(
                                &room_id,
                                &command.content,
                                command.media.as_ref(),
                                &command.display_name,
                                &command.author_public_key,
                                &command.author_signature,
                                &command.author_challenge,
                                &command.site_id,
                                command.reply_to.as_deref(),
                                reply_to_body.as_deref(),
                                reply_to_sender.as_deref(),
                                Some(id),
                                pending.force_new_txn,
                            )
                            .await
                    };
                    match result {
                        Ok(event_id) => event_id,
                        Err(e) => {
                            // The room may have been tombstoned or its alias
                            // moved: retire the registry entry so the next
                            // retry goes through alias recovery and adopts
                            // the successor.
                            if is_room_gone(&e) {
                                warn!(
                                    "Post submission [{}] failed on room {}; retiring registry entry: {:#}",
                                    id, room_id, e
                                );
                                let _ = self.deps.registry_store.retire_room(&room_id).await;
                            }
                            return Err(e);
                        }
                    }
                };
                // The media is now referenced by a real room event; mark it
                // used so orphan cleanup does not delete it.
                if let Some(media) = &command.media {
                    let _ = self.deps.message_store.mark_media_used(&media.url).await;
                }

                // 5. Closed-loop: Mark as waiting for sync instead of completed
                self.deps
                    .submission_store
                    .mark_post_submission_waiting_for_sync(id, &event_id, &room_id)
                    .await?;

                Ok(())
            })
            .await;

            if let Err(e) = process_result {
                let retrying = self
                    .deps
                    .submission_store
                    .record_post_submission_failure(id, &e.to_string())
                    .await?;
                if retrying {
                    warn!(
                        "Post submission [{}] failed, will retry after backoff: {:?}",
                        id, e
                    );
                } else {
                    error!(
                        "Post submission [{}] exhausted retries, moved to failed: {:?}",
                        id, e
                    );
                }
            }
        }
        Ok(num_submissions)
    }
}

#[async_trait]
impl ReconcilePass for PostsPass {
    fn config(&self) -> &PassConfig {
        &self.config
    }

    async fn run(&self) -> Result<u64> {
        self.reconcile().await
    }
}
