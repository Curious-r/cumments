//! Update-command reconciliation: locate the room and send the m.replace.

use super::*;
use anyhow::Result;
use async_trait::async_trait;
use tracing::{error, warn};

/// Reconciles pending update (edit) submissions toward Matrix.
pub struct UpdatesPass {
    deps: Arc<ReconcilerDeps>,
    config: PassConfig,
}

impl UpdatesPass {
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
            .claim_pending_update_submissions(SUBMISSION_BATCH_SIZE, lease_until)
            .await?;

        if submissions.is_empty() {
            return Ok(0);
        }
        let num_submissions = submissions.len() as u64;

        for pending in submissions {
            let id = pending.id;
            let command = pending.command;
            let process_result = run_submission(async {
                // 1. Brain: Ensure site space
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

                // 3. Hands: Ensure room
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

                // 3b. Registry: Write back the room mapping immediately
                self.deps
                    .registry_store
                    .register_room(&room_id, &command.site_id, &command.post_slug)
                    .await?;

                // 4. Hands: Fetch original display name to maintain it
                let display_name = self
                    .deps
                    .message_store
                    .get_author_display_name(&command.event_id)
                    .await?
                    .flatten()
                    .unwrap_or_else(|| "Guest".to_string());

                // 5. Hands: Perform the update (m.replace)
                match self
                    .deps
                    .driver
                    .update_message(
                        &room_id,
                        &command.event_id,
                        &command.content,
                        &display_name,
                        &command.author_public_key,
                        &command.author_signature,
                        &command.author_challenge,
                        &command.site_id,
                        Some(id),
                    )
                    .await
                {
                    Ok(_) => {}
                    Err(e) => {
                        if is_room_gone(&e) {
                            warn!(
                                "Update submission [{}] failed on room {}; retiring registry entry: {:#}",
                                id, room_id, e
                            );
                            let _ = self.deps.registry_store.retire_room(&room_id).await;
                        }
                        return Err(e);
                    }
                }

                // 6. Closed-loop: Mark as waiting for sync
                self.deps
                    .submission_store
                    .mark_update_submission_waiting_for_sync(id, &room_id)
                    .await?;

                Ok(())
            })
            .await;

            if let Err(e) = process_result {
                let retrying = self
                    .deps
                    .submission_store
                    .record_update_submission_failure(id, &e.to_string())
                    .await?;
                if retrying {
                    warn!(
                        "Update submission [{}] failed, will retry after backoff: {:?}",
                        id, e
                    );
                } else {
                    error!(
                        "Update submission [{}] exhausted retries, moved to failed: {:?}",
                        id, e
                    );
                }
            }
        }
        Ok(num_submissions)
    }
}

#[async_trait]
impl ReconcilePass for UpdatesPass {
    fn config(&self) -> &PassConfig {
        &self.config
    }

    async fn run(&self) -> Result<u64> {
        self.reconcile().await
    }
}
