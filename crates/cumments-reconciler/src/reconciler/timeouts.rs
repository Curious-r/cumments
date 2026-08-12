//! Timeout reconciliation for intents stuck in `waiting_for_sync`.

use super::*;
use anyhow::Result;
use tracing::{error, warn};

impl Reconciler {
    /// Handles intents stuck in `waiting_for_sync`: the event was sent but the
    /// projector never observed it (lost push, misconfigured AS, ...).
    ///
    /// - Post intents are only rescheduled when the event is confirmed absent
    ///   on the homeserver; if the event exists, resending would duplicate the
    ///   comment, so the intent is dead-lettered for inspection.
    /// - Delete/update intents are rescheduled directly: redaction and
    ///   replacement are idempotent operations.
    pub(super) async fn reconcile_timeouts(&self) -> Result<u64> {
        let cutoff =
            chrono::Utc::now() - chrono::Duration::minutes(WAITING_FOR_SYNC_TIMEOUT_MINUTES);
        let mut handled = 0u64;

        loop {
            let stuck_batch = self
                .intent_store
                .get_stuck_post_intents(cutoff, INTENT_BATCH_SIZE)
                .await?;
            if stuck_batch.is_empty() {
                break;
            }
            for stuck in stuck_batch {
                let id = stuck.id;
                let event_id = stuck.event_id;
                let room_id = stuck.room_id;
                handled += 1;

                let Some(room_id) = room_id else {
                    error!(
                        "Post intent [{}] timed out with no room recorded; dead-lettering",
                        id
                    );
                    self.intent_store
                    .dead_letter_post_intent(
                        id,
                        "waiting_for_sync timed out; room_id was not recorded, cannot verify the event safely",
                    )
                    .await?;
                    continue;
                };

                if event_id.is_empty() {
                    error!(
                        "Post intent [{}] timed out with no event id recorded; dead-lettering",
                        id
                    );
                    self.intent_store
                    .dead_letter_post_intent(
                        id,
                        "waiting_for_sync timed out; event_id was not recorded, cannot verify the event safely",
                    )
                    .await?;
                    continue;
                }

                match self.driver.event_exists(&room_id, &event_id).await {
                    Ok(true) => {
                        self.intent_store.reset_post_timeout_errors(id).await?;
                        let confirmations = self
                            .intent_store
                            .increment_post_timeout_confirmation(id)
                            .await?;
                        if confirmations >= TIMEOUT_CONFIRMATION_LIMIT {
                            error!(
                                "Post intent [{}] timed out and event {} exists on the homeserver; dead-lettering after {confirmations} confirmations",
                                id, event_id
                            );
                            self.intent_store
                            .dead_letter_post_intent(
                                id,
                                &format!(
                                    "waiting_for_sync timed out; event {} exists on the homeserver but was never projected after {confirmations} confirmation passes",
                                    event_id
                                ),
                            )
                            .await?;
                        } else {
                            warn!(
                                "Post intent [{}] event {} exists but projection is delayed; confirmation {confirmations}/{TIMEOUT_CONFIRMATION_LIMIT}",
                                id, event_id
                            );
                        }
                    }
                    Ok(false) => {
                        self.intent_store.reset_post_timeout_errors(id).await?;
                        warn!(
                            "Post intent [{}] timed out and event {} is absent; rescheduling",
                            id, event_id
                        );
                        self.intent_store
                            .reset_post_timeout_confirmations(id)
                            .await?;
                        let retrying = self
                            .intent_store
                            .record_post_intent_failure(
                                id,
                                "waiting_for_sync timed out; event absent, resending",
                            )
                            .await?;
                        if !retrying {
                            error!("Post intent [{}] exhausted retries after timeout", id);
                        }
                    }
                    Err(e) => {
                        let errors = self.intent_store.increment_post_timeout_error(id).await?;
                        if errors >= TIMEOUT_ERROR_LIMIT {
                            error!(
                                "Post intent [{}] timeout check failed {errors} times; dead-lettering: {:?}",
                                id, e
                            );
                            self.intent_store
                            .dead_letter_post_intent(
                                id,
                                &format!(
                                    "waiting_for_sync timeout check failed {errors} consecutive times: {e}"
                                ),
                            )
                            .await?;
                        } else {
                            warn!(
                                "Post intent [{}] timeout check failed (error {errors}/{TIMEOUT_ERROR_LIMIT}): {:?}",
                                id, e
                            );
                        }
                    }
                }
            }
        }

        // Redaction and replacement are idempotent, so rescheduling is safe.
        loop {
            let ids = self
                .intent_store
                .get_stuck_delete_intent_ids(cutoff, INTENT_BATCH_SIZE)
                .await?;
            if ids.is_empty() {
                break;
            }
            for id in ids {
                handled += 1;
                let retrying = self
                    .intent_store
                    .record_delete_intent_failure(id, "waiting_for_sync timed out; rescheduling")
                    .await?;
                if !retrying {
                    error!("Delete intent [{}] exhausted retries after timeout", id);
                }
            }
        }

        loop {
            let ids = self
                .intent_store
                .get_stuck_update_intent_ids(cutoff, INTENT_BATCH_SIZE)
                .await?;
            if ids.is_empty() {
                break;
            }
            for id in ids {
                handled += 1;
                let retrying = self
                    .intent_store
                    .record_update_intent_failure(id, "waiting_for_sync timed out; rescheduling")
                    .await?;
                if !retrying {
                    error!("Update intent [{}] exhausted retries after timeout", id);
                }
            }
        }

        Ok(handled)
    }
}
