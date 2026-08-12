//! Update-intent reconciliation: locate the room and send the m.replace.

use super::*;
use anyhow::Result;
use tracing::{error, warn};

impl Reconciler {
    pub(super) async fn reconcile_updates(&self) -> Result<u64> {
        let intents = self
            .intent_store
            .get_pending_update_intents(INTENT_BATCH_SIZE)
            .await?;

        if intents.is_empty() {
            return Ok(0);
        }
        let num_intents = intents.len() as u64;

        for pending in intents {
            let id = pending.id;
            let intent = pending.intent;
            let process_result = run_intent(async {
                // 1. Brain: Ensure site space
                let space_id = self
                    .site_service
                    .ensure_space(&intent.site_id, self.driver.as_ref())
                    .await?;

                // 2. Registry: Check for existing room in local cache (O(1) hint)
                let candidate_room_id = self
                    .registry_store
                    .get_registered_room(&intent.site_id, &intent.post_slug)
                    .await?;

                // 3. Hands: Ensure room
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
                                .mark_room_blocked(candidate, &e.to_string())
                                .await;
                        }
                        return Err(e);
                    }
                };

                // 3b. Registry: Write back the room mapping immediately
                self.registry_store
                    .register_room(&room_id, &intent.site_id, &intent.post_slug)
                    .await?;

                // 4. Hands: Fetch original display name to maintain it
                let display_name = self
                    .comment_store
                    .get_author_display_name(&intent.event_id)
                    .await?
                    .flatten()
                    .unwrap_or_else(|| "Guest".to_string());

                // 5. Hands: Perform the update (m.replace)
                match self
                    .driver
                    .update_message(
                        &room_id,
                        &intent.event_id,
                        &intent.content,
                        &display_name,
                        &intent.author_public_key,
                        &intent.author_signature,
                        &intent.author_challenge,
                        &intent.site_id,
                        Some(id),
                    )
                    .await
                {
                    Ok(_) => {}
                    Err(e) => {
                        if room_unavailable(&e) {
                            warn!(
                                "Update intent [{}] failed on room {}; invalidating registry entry: {:#}",
                                id, room_id, e
                            );
                            let _ = self
                                .registry_store
                                .invalidate_room_registry(&room_id)
                                .await;
                        }
                        return Err(e);
                    }
                }

                // 6. Closed-loop: Mark as waiting for sync
                self.intent_store
                    .mark_update_intent_waiting_for_sync(id, &room_id)
                    .await?;

                Ok(())
            })
            .await;

            if let Err(e) = process_result {
                let retrying = self
                    .intent_store
                    .record_update_intent_failure(id, &e.to_string())
                    .await?;
                if retrying {
                    warn!(
                        "Update intent [{}] failed, will retry after backoff: {:?}",
                        id, e
                    );
                } else {
                    error!(
                        "Update intent [{}] exhausted retries, moved to failed: {:?}",
                        id, e
                    );
                }
            }
        }
        Ok(num_intents)
    }
}
