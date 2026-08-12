//! Post-intent reconciliation: ensure the comment room, then send.

use super::*;
use anyhow::Result;
use tracing::{error, warn};

impl Reconciler {
    pub(super) async fn reconcile_posts(&self) -> Result<u64> {
        let intents = self
            .intent_store
            .get_pending_post_intents(INTENT_BATCH_SIZE)
            .await?;

        if intents.is_empty() {
            return Ok(0);
        }
        let num_intents = intents.len() as u64;

        for pending in intents {
            let id = pending.id;
            let intent = pending.intent;
            let process_result = run_intent(async {
                // ORCHESTRATION START
                // 1. Brain: Ensure the site space is ready
                let space_id = self
                    .site_service
                    .ensure_space(&intent.site_id, self.driver.as_ref())
                    .await?;

                // 2. Registry: Check for existing room in local cache (O(1) hint)
                let candidate_room_id = self
                    .registry_store
                    .get_registered_room(&intent.site_id, &intent.post_slug)
                    .await?;

                // 3. Hands: Ensure the post-specific room exists and is linked
                let room_id = match self
                    .driver
                    .ensure_comment_room(
                        &intent.site_id,
                        &intent.post_slug,
                        &space_id,
                        candidate_room_id.as_deref(),
                    )
                    .await
                {
                    Ok(room_id) => room_id,
                    Err(e) => {
                        if adoption_blocked(&e)
                            && let Some(candidate) = candidate_room_id.as_deref()
                        {
                            let _ = self
                                .registry_store
                                .quarantine_room(candidate, &e.to_string(), None)
                                .await;
                        }
                        return Err(e);
                    }
                };

                // 3b. Registry: Write back the room mapping immediately
                // This is critical in AppService mode: pushes resolve room
                // identity from this registry, so the mapping must be durable
                // before any event can arrive.
                self.registry_store
                    .register_room(&room_id, &intent.site_id, &intent.post_slug)
                    .await?;

                // 3c. Resolve the replied-to comment from the read model so
                // the Matrix message can carry a rich-reply fallback quote.
                // Unknown originals (e.g. replies to a not-yet-projected
                // event) simply skip the quote.
                let (reply_to_body, reply_to_sender) = match intent.reply_to.as_deref() {
                    Some(event_id) => match self.comment_store.get_comment(event_id).await? {
                        Some(comment) => (Some(comment.content), Some(comment.sender_mxid)),
                        None => (None, None),
                    },
                    None => (None, None),
                };

                // 4. Hands: Post the actual message
                let event_id = match self
                    .driver
                    .post_message(
                        &room_id,
                        &intent.content,
                        &intent.display_name,
                        &intent.author_public_key,
                        &intent.author_signature,
                        &intent.author_challenge,
                        &intent.site_id,
                        intent.reply_to.as_deref(),
                        reply_to_body.as_deref(),
                        reply_to_sender.as_deref(),
                        Some(id),
                    )
                    .await
                {
                    Ok(event_id) => event_id,
                    Err(e) => {
                        // The room may have been tombstoned or its alias moved:
                        // drop the registry hint so the next retry goes through
                        // alias recovery and adopts the successor room.
                        if room_unavailable(&e) {
                            warn!(
                                "Post intent [{}] failed on room {}; invalidating registry entry: {:#}",
                                id, room_id, e
                            );
                            let _ = self
                                .registry_store
                                .retire_room(&room_id)
                                .await;
                        }
                        return Err(e);
                    }
                };

                // 5. Closed-loop: Mark as waiting for sync instead of completed
                self.intent_store
                    .mark_post_intent_waiting_for_sync(id, &event_id, &room_id)
                    .await?;
                // ORCHESTRATION END

                Ok(())
            })
            .await;

            if let Err(e) = process_result {
                let retrying = self
                    .intent_store
                    .record_post_intent_failure(id, &e.to_string())
                    .await?;
                if retrying {
                    warn!(
                        "Post intent [{}] failed, will retry after backoff: {:?}",
                        id, e
                    );
                } else {
                    error!(
                        "Post intent [{}] exhausted retries, moved to failed: {:?}",
                        id, e
                    );
                }
            }
        }
        Ok(num_intents)
    }
}
