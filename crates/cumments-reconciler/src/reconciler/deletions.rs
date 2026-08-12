//! Delete-intent reconciliation: locate the room and redact the event.

use super::*;
use anyhow::Result;
use cumments_core::protocol::REDACTION_PROOF_KEY;
use tracing::{error, warn};

impl Reconciler {
    pub(super) async fn reconcile_deletions(&self) -> Result<u64> {
        let intents = self
            .intent_store
            .get_pending_delete_intents(INTENT_BATCH_SIZE)
            .await?;

        if intents.is_empty() {
            return Ok(0);
        }
        let num_intents = intents.len() as u64;

        for pending in intents {
            let id = pending.id;
            let intent = pending.intent;
            let process_result = run_intent(async {
                // 1. Brain: Ensure the site space is ready (same as posts).
                let space_id = self
                    .site_service
                    .ensure_space(&intent.site_id, self.driver.as_ref())
                    .await?;

                // 2. Registry: Locate the room ID for this deletion.
                let candidate_room_id = self
                    .registry_store
                    .get_registered_room(&intent.site_id, &intent.post_slug)
                    .await?;

                // 3. Hands: Recover/adopt the room when the registry is stale
                // or missing, mirroring the post/update paths.
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
                self.registry_store
                    .register_room(&room_id, &intent.site_id, &intent.post_slug)
                    .await?;

                // 4. Hands: Perform the redaction
                let proof = serde_json::json!({
                    (REDACTION_PROOF_KEY): {
                        "site_id": intent.site_id.as_str(),
                        "post_slug": intent.post_slug.as_str(),
                        "target_event_id": intent.event_id.as_str(),
                        "public_key": intent.author_public_key.as_str(),
                        "signature": intent.author_signature.as_str(),
                        "challenge": intent.author_challenge.as_str(),
                        "intent_id": id,
                    }
                });
                match self
                    .driver
                    .redact_message(&room_id, &intent.event_id, Some(id), Some(&proof))
                    .await
                {
                    Ok(()) => {}
                    Err(e) => {
                        // The room may have been tombstoned or its alias moved:
                        // drop the registry hint so the next retry re-discovers.
                        if room_unavailable(&e) {
                            warn!(
                                "Delete intent [{}] failed on room {}; invalidating registry entry: {:#}",
                                id, room_id, e
                            );
                            let _ = self
                                .registry_store
                                .retire_room(&room_id)
                                .await;
                        }
                        return Err(e);
                    }
                }

                // 5. Concepts: Move to waiting_for_sync
                self.intent_store
                    .mark_delete_intent_waiting_for_sync(id, &room_id)
                    .await?;

                Ok(())
            })
            .await;

            if let Err(e) = process_result {
                let retrying = self
                    .intent_store
                    .record_delete_intent_failure(id, &e.to_string())
                    .await?;
                if retrying {
                    warn!(
                        "Delete intent [{}] failed, will retry after backoff: {:?}",
                        id, e
                    );
                } else {
                    error!(
                        "Delete intent [{}] exhausted retries, moved to failed: {:?}",
                        id, e
                    );
                }
            }
        }
        Ok(num_intents)
    }
}
