//! Delete-command reconciliation: locate the room and redact the event.

use super::*;
use anyhow::Result;
use async_trait::async_trait;
use cumments_core::protocol::REDACTION_PROOF_KEY;
use tracing::{error, warn};

/// Reconciles pending delete submissions toward Matrix.
pub struct DeletionsPass {
    deps: Arc<ReconcilerDeps>,
    config: PassConfig,
}

impl DeletionsPass {
    pub fn new(deps: Arc<ReconcilerDeps>, config: PassConfig) -> Self {
        Self { deps, config }
    }

    async fn reconcile(&self) -> Result<u64> {
        let submissions = self
            .deps
            .submission_store
            .get_pending_delete_submissions(SUBMISSION_BATCH_SIZE)
            .await?;

        if submissions.is_empty() {
            return Ok(0);
        }
        let num_submissions = submissions.len() as u64;

        for pending in submissions {
            let id = pending.id;
            let command = pending.command;
            let process_result = run_submission(async {
                // 1. Brain: Ensure the site space is ready (same as posts).
                let space_id = self
                    .deps
                    .site_service
                    .ensure_space(&command.site_id, self.deps.driver.as_ref())
                    .await?;

                // 2. Registry: Locate the room ID for this deletion.
                let candidate_room_id = self
                    .deps
                    .registry_store
                    .get_registered_room(&command.site_id, &command.post_slug)
                    .await?;

                // Quarantine gate: fail fast while a quarantined room's retry
                // is not due instead of hammering alias recovery per comment.
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

                // 3. Hands: Recover/adopt the room when the registry is stale
                // or missing, mirroring the post/update paths.
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
                            let room_id = quarantine_target(&e)
                                .or_else(|| candidate_room_id.clone());
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
                self.deps
                    .registry_store
                    .register_room(&room_id, &command.site_id, &command.post_slug)
                    .await?;

                // 4. Hands: Perform the redaction
                let proof = serde_json::json!({
                    (REDACTION_PROOF_KEY): {
                        "site_id": command.site_id.as_str(),
                        "post_slug": command.post_slug.as_str(),
                        "target_event_id": command.event_id.as_str(),
                        "public_key": command.author_public_key.as_str(),
                        "signature": command.author_signature.as_str(),
                        "challenge": command.author_challenge.as_str(),
                        "submission_id": id,
                    }
                });
                match self
                    .deps
                    .driver
                    .redact_message(&room_id, &command.event_id, Some(id), Some(&proof))
                    .await
                {
                    Ok(()) => {}
                    Err(e) => {
                        if is_room_gone(&e) {
                            warn!(
                                "Delete submission [{}] failed on room {}; retiring registry entry: {:#}",
                                id, room_id, e
                            );
                            let _ = self.deps.registry_store.retire_room(&room_id).await;
                        }
                        return Err(e);
                    }
                }

                // 5. Concepts: Move to waiting_for_sync
                self.deps
                    .submission_store
                    .mark_delete_submission_waiting_for_sync(id, &room_id)
                    .await?;

                Ok(())
            })
            .await;

            if let Err(e) = process_result {
                let retrying = self
                    .deps
                    .submission_store
                    .record_delete_submission_failure(id, &e.to_string())
                    .await?;
                if retrying {
                    warn!(
                        "Delete submission [{}] failed, will retry after backoff: {:?}",
                        id, e
                    );
                } else {
                    error!(
                        "Delete submission [{}] exhausted retries, moved to failed: {:?}",
                        id, e
                    );
                }
            }
        }
        Ok(num_submissions)
    }
}

#[async_trait]
impl ReconcilePass for DeletionsPass {
    fn config(&self) -> &PassConfig {
        &self.config
    }

    async fn run(&self) -> Result<u64> {
        self.reconcile().await
    }
}
